use {
    reqwest::{
        dns::{Addrs, Name, Resolve, Resolving},
        Url,
    },
    std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("invalid endpoint url: {0}")]
    InvalidUrl(String),
    #[error("endpoint scheme must be http or https")]
    InvalidScheme,
    #[error("endpoint must have a host")]
    MissingHost,
    #[error("endpoint host could not be resolved: {0}")]
    Unresolvable(String),
    #[error("endpoint resolves to a non-public address: {0}")]
    ForbiddenAddress(IpAddr),
}

/// Whether `ip` is a non-public address (loopback, private, link-local,
/// unspecified, multicast, ...) that webhooks must not target.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_forbidden_v4(ip),
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_forbidden_v4(v4);
            }
            is_forbidden_v6(ip)
        }
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        // Carrier-grade NAT 100.64.0.0/10
        || (a == 100 && (b & 0xc0) == 64)
        // Reserved 240.0.0.0/4
        || a >= 240
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // Unique local fc00::/7
        || (first & 0xfe00) == 0xfc00
        // Link-local fe80::/10
        || (first & 0xffc0) == 0xfe80
}

/// Parse and validate a webhook endpoint. Unless `allow_private` is set, every
/// address the host resolves to must be public.
pub async fn validate_endpoint(endpoint: &str, allow_private: bool) -> Result<Url, EndpointError> {
    let url = Url::parse(endpoint).map_err(|e| EndpointError::InvalidUrl(e.to_string()))?;

    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(EndpointError::InvalidScheme);
    }

    let host = url.host_str().ok_or(EndpointError::MissingHost)?;
    if allow_private {
        return Ok(url);
    }

    let port = url.port_or_known_default().unwrap_or(80);
    // Strip IPv6 brackets for resolution.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| EndpointError::Unresolvable(e.to_string()))?
        .collect();

    if addrs.is_empty() {
        return Err(EndpointError::Unresolvable(host.to_string()));
    }

    if let Some(addr) = addrs.iter().find(|addr| is_forbidden_ip(addr.ip())) {
        return Err(EndpointError::ForbiddenAddress(addr.ip()));
    }

    Ok(url)
}

/// DNS resolver for the webhook HTTP client that drops non-public addresses,
/// so a host cannot be re-pointed at internal services after validation.
pub struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| !is_forbidden_ip(addr.ip()))
                .collect();

            if addrs.is_empty() {
                return Err(format!("{host} has no public addresses").into());
            }

            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forbidden_ips() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }

        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn rejects_bad_endpoints() {
        assert!(matches!(
            validate_endpoint("ftp://example.com", false).await,
            Err(EndpointError::InvalidScheme)
        ));
        assert!(matches!(
            validate_endpoint("http://127.0.0.1:8332/", false).await,
            Err(EndpointError::ForbiddenAddress(_))
        ));
        assert!(matches!(
            validate_endpoint("http://[::1]/", false).await,
            Err(EndpointError::ForbiddenAddress(_))
        ));
        assert!(validate_endpoint("http://127.0.0.1:8332/", true)
            .await
            .is_ok());
    }
}
