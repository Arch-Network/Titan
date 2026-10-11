use {
    reqwest::{
        dns::{Addrs, Name, Resolve, Resolving},
        Url,
    },
    std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

/// Longest accepted endpoint URL, in bytes.
const MAX_ENDPOINT_LENGTH: usize = 2048;

#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("endpoint exceeds the {0}-byte limit")]
    TooLong(usize),
    #[error("invalid endpoint url: {0}")]
    InvalidUrl(String),
    #[error("endpoint scheme must be http or https")]
    InvalidScheme,
    #[error("endpoint must not contain credentials")]
    CredentialsNotAllowed,
    #[error("endpoint must have a host")]
    MissingHost,
    #[error("endpoint host could not be resolved: {0}")]
    Unresolvable(String),
    #[error("endpoint resolves to a non-public address: {0}")]
    ForbiddenAddress(IpAddr),
}

/// Whether `ip` is not a publicly routable unicast address (loopback, private,
/// link-local, special-use, multicast, ...) and so must not be a webhook target.
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_forbidden_v4(ip),
        IpAddr::V6(ip) => is_forbidden_v6(ip),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    matches!(
        ip.octets(),
        [0, ..]
            | [10, ..]
            // Carrier-grade NAT 100.64.0.0/10
            | [100, 64..=127, ..]
            | [127, ..]
            | [169, 254, ..]
            | [172, 16..=31, ..]
            // IETF protocol assignments
            | [192, 0, 0, _]
            | [192, 0, 2, _]
            // 6to4 relay anycast
            | [192, 88, 99, _]
            | [192, 168, ..]
            // Benchmarking 198.18.0.0/15
            | [198, 18..=19, ..]
            | [198, 51, 100, _]
            | [203, 0, 113, _]
            // Multicast, reserved and broadcast
            | [224..=255, ..]
    )
}

/// Only global unicast (2000::/3) is allowed, minus special-use blocks that can
/// tunnel to or embed arbitrary IPv4 addresses.
fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    // IPv4-mapped and IPv4-compatible addresses (also covers `::` and `::1`).
    if let Some(v4) = ip.to_ipv4() {
        return is_forbidden_v4(v4);
    }

    let [first, second, ..] = ip.segments();
    !(0x2000..=0x3fff).contains(&first)
        // IETF protocol assignments 2001::/23, including Teredo
        || (first == 0x2001 && second < 0x0200)
        // Documentation 2001:db8::/32
        || (first == 0x2001 && second == 0x0db8)
        // 6to4 2002::/16
        || first == 0x2002
        // Documentation 3fff::/20
        || (first == 0x3fff && second < 0x1000)
}

/// Parse and validate a webhook endpoint. Unless `allow_private` is set, every
/// address the host resolves to must be public.
pub async fn validate_endpoint(endpoint: &str, allow_private: bool) -> Result<Url, EndpointError> {
    if endpoint.len() > MAX_ENDPOINT_LENGTH {
        return Err(EndpointError::TooLong(MAX_ENDPOINT_LENGTH));
    }

    let url = Url::parse(endpoint).map_err(|e| EndpointError::InvalidUrl(e.to_string()))?;

    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(EndpointError::InvalidScheme);
    }

    // Subscriptions are listed back to admins; never store secrets in them.
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::CredentialsNotAllowed);
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
            "192.0.0.8",
            "198.18.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::10.0.0.1",
            // NAT64 can reach IPv4 private ranges.
            "64:ff9b::a00:1",
            // 6to4 embedding 10.0.0.1
            "2002:a00:1::",
            // Teredo
            "2001::1",
            "2001:db8::1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }

        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "::ffff:8.8.8.8",
            "2606:4700:4700::1111",
        ] {
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
        assert!(matches!(
            validate_endpoint("https://user:secret@example.com/hook", false).await,
            Err(EndpointError::CredentialsNotAllowed)
        ));
        let long = format!("https://example.com/{}", "a".repeat(MAX_ENDPOINT_LENGTH));
        assert!(matches!(
            validate_endpoint(&long, false).await,
            Err(EndpointError::TooLong(_))
        ));
        assert!(validate_endpoint("http://127.0.0.1:8332/", true)
            .await
            .is_ok());
    }
}
