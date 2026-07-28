use {
    super::store::Store,
    arch_titan_types_core::{Event, EventType},
    reqwest::{redirect::Policy, Client, StatusCode, Url},
    std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
    thiserror::Error,
    tokio::{
        net::lookup_host,
        sync::Semaphore,
        time::{sleep, timeout},
    },
    tracing::error,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ENDPOINT_LENGTH: usize = 2048;
const MAX_RESOLVED_ADDRESSES: usize = 16;
const MAX_WEBHOOK_DELIVERY_ATTEMPTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum EndpointError {
    #[error("webhook endpoint exceeds the {max}-byte limit")]
    TooLong { max: usize },
    #[error("invalid webhook endpoint URL: {0}")]
    InvalidUrl(String),
    #[error("webhook endpoint scheme must be http or https")]
    UnsupportedScheme,
    #[error("webhook endpoint credentials are not allowed")]
    CredentialsNotAllowed,
    #[error("webhook endpoint has no host")]
    MissingHost,
    #[error("webhook endpoint host is not publicly routable: {0}")]
    HostNotPublic(String),
    #[error("webhook endpoint has no usable port")]
    MissingPort,
    #[error("DNS lookup timed out for webhook endpoint host: {0}")]
    DnsTimeout(String),
    #[error("DNS lookup failed for webhook endpoint host {host}: {reason}")]
    DnsLookup { host: String, reason: String },
    #[error("DNS lookup returned no addresses for webhook endpoint host: {0}")]
    NoAddresses(String),
    #[error("DNS lookup returned more than {max} addresses for webhook endpoint host: {host}")]
    TooManyAddresses { host: String, max: usize },
    #[error("failed to build hardened webhook client: {0}")]
    ClientBuild(String),
}

#[derive(Debug, Error)]
enum SendEventError {
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    #[error("failed to build webhook request: {0}")]
    RequestBuild(#[source] reqwest::Error),
    #[error("webhook request body cannot be replayed")]
    RequestNotCloneable,
    #[error("webhook request failed after {attempts} attempt(s): {last_error}")]
    Failed { attempts: usize, last_error: String },
}

struct ResolvedEndpoint {
    url: Url,
    domain: Option<String>,
    addresses: Vec<SocketAddr>,
}

struct ValidatedEndpoint {
    url: Url,
    host: String,
    normalized_host: String,
    port: u16,
}

impl ResolvedEndpoint {
    fn client(&self) -> Result<Client, EndpointError> {
        let mut builder = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .no_proxy();

        if let Some(domain) = &self.domain {
            builder = builder.resolve_to_addrs(domain, &self.addresses);
        }

        builder
            .build()
            .map_err(|error| EndpointError::ClientBuild(error.to_string()))
    }
}

pub async fn validate_webhook_endpoint(endpoint: &str) -> Result<(), EndpointError> {
    resolve_webhook_endpoint(endpoint).await.map(drop)
}

fn normalize_host(host: &str) -> String {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn parse_webhook_url(endpoint: &str) -> Result<ValidatedEndpoint, EndpointError> {
    if endpoint.len() > MAX_ENDPOINT_LENGTH {
        return Err(EndpointError::TooLong {
            max: MAX_ENDPOINT_LENGTH,
        });
    }

    let url = Url::parse(endpoint).map_err(|error| EndpointError::InvalidUrl(error.to_string()))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(EndpointError::UnsupportedScheme);
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::CredentialsNotAllowed);
    }

    let host = url
        .host_str()
        .ok_or(EndpointError::MissingHost)?
        .to_string();
    let normalized_host = normalize_host(&host);

    if normalized_host == "localhost" || normalized_host.ends_with(".localhost") {
        return Err(EndpointError::HostNotPublic(host));
    }

    if let Ok(address) = normalized_host.parse::<IpAddr>() {
        ensure_public_ip(address, &host)?;
    }

    let port = url
        .port_or_known_default()
        .ok_or(EndpointError::MissingPort)?;

    Ok(ValidatedEndpoint {
        url,
        host,
        normalized_host,
        port,
    })
}

pub(super) fn validate_webhook_url(endpoint: &str) -> Result<(), EndpointError> {
    parse_webhook_url(endpoint).map(drop)
}

async fn resolve_webhook_endpoint(endpoint: &str) -> Result<ResolvedEndpoint, EndpointError> {
    let ValidatedEndpoint {
        url,
        host,
        normalized_host,
        port,
    } = parse_webhook_url(endpoint)?;

    if let Ok(address) = normalized_host.parse::<IpAddr>() {
        return Ok(ResolvedEndpoint {
            url,
            domain: None,
            addresses: vec![SocketAddr::new(address, port)],
        });
    }

    let resolved = timeout(
        DNS_LOOKUP_TIMEOUT,
        lookup_host((normalized_host.as_str(), port)),
    )
    .await
    .map_err(|_| EndpointError::DnsTimeout(host.clone()))?
    .map_err(|error| EndpointError::DnsLookup {
        host: host.clone(),
        reason: error.to_string(),
    })?;

    let mut addresses = Vec::new();
    for address in resolved {
        ensure_public_ip(address.ip(), &host)?;

        if addresses.contains(&address) {
            continue;
        }

        if addresses.len() == MAX_RESOLVED_ADDRESSES {
            return Err(EndpointError::TooManyAddresses {
                host,
                max: MAX_RESOLVED_ADDRESSES,
            });
        }

        addresses.push(address);
    }

    if addresses.is_empty() {
        return Err(EndpointError::NoAddresses(host));
    }

    Ok(ResolvedEndpoint {
        url,
        domain: Some(host),
        addresses,
    })
}

fn ensure_public_ip(address: IpAddr, host: &str) -> Result<(), EndpointError> {
    let is_public = match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    };

    if is_public {
        Ok(())
    } else {
        Err(EndpointError::HostNotPublic(host.to_string()))
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();

    !matches!(
        (first, second, third),
        (0, _, _)
            | (10, _, _)
            | (100, 64..=127, _)
            | (127, _, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 0, 0)
            | (192, 0, 2)
            | (192, 88, 99)
            | (192, 168, _)
            | (198, 18..=19, _)
            | (198, 51, 100)
            | (203, 0, 113)
            | (224..=255, _, _)
    )
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(address) = address.to_ipv4() {
        return is_public_ipv4(address);
    }

    let segments = address.segments();

    (0x2000..=0x3fff).contains(&segments[0])
        && !(segments[0] == 0x2001 && segments[1] < 0x0200)
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        && segments[0] != 0x2002
        && !(segments[0] == 0x3fff && segments[1] < 0x1000)
}

async fn send_event_with_retry(endpoint: &str, event: &Event) -> Result<(), SendEventError> {
    let resolved_endpoint = resolve_webhook_endpoint(endpoint).await?;
    let client = resolved_endpoint.client()?;
    let request = client
        .post(resolved_endpoint.url.as_str())
        .json(event)
        .build()
        .map_err(SendEventError::RequestBuild)?;
    let mut last_error = String::new();

    for attempt in 1..=MAX_WEBHOOK_DELIVERY_ATTEMPTS {
        let request = request
            .try_clone()
            .ok_or(SendEventError::RequestNotCloneable)?;

        match client.execute(request).await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                let status = response.status();
                last_error = format!("HTTP status {status}");

                if status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS {
                    return Err(SendEventError::Failed {
                        attempts: attempt,
                        last_error,
                    });
                }
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }

        if attempt < MAX_WEBHOOK_DELIVERY_ATTEMPTS {
            sleep(Duration::from_secs(2_u64.pow(attempt as u32))).await;
        }
    }

    Err(SendEventError::Failed {
        attempts: MAX_WEBHOOK_DELIVERY_ATTEMPTS,
        last_error,
    })
}

/// Process an event and send it to all interested webhook subscriptions.
pub async fn process_event(
    store: &Arc<dyn Store>,
    delivery_semaphore: &Arc<Semaphore>,
    event: &Event,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_type = EventType::from(event.clone());
    let subscriptions = store.get_subscriptions()?;

    for subscription in subscriptions
        .into_iter()
        .filter(|subscription| subscription.event_types.contains(&event_type))
    {
        let permit = delivery_semaphore.clone().acquire_owned().await?;
        let endpoint = subscription.endpoint;
        let subscription_id = subscription.id;
        let event = event.clone();
        let store = store.clone();

        tokio::spawn(async move {
            let _permit = permit;
            match send_event_with_retry(&endpoint, &event).await {
                Ok(()) => {
                    let now_secs = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let _ = store.update_subscription_last_success(&subscription_id, now_secs);
                }
                Err(error) => {
                    error!("Webhook delivery to {endpoint} failed: {error}");
                }
            }
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_webhook_endpoints() {
        assert!(validate_webhook_url("https://8.8.8.8/webhook").is_ok());
        assert!(validate_webhook_url("https://[2606:4700:4700::1111]/webhook").is_ok());
        assert!(validate_webhook_url("https://example.com/webhook").is_ok());

        let trailing_dot = parse_webhook_url("https://example.com./webhook").unwrap();
        assert_eq!(trailing_dot.host, "example.com.");
        assert_eq!(trailing_dot.normalized_host, "example.com");
    }

    #[test]
    fn rejects_non_http_and_credentialed_endpoints() {
        assert!(matches!(
            validate_webhook_url("file:///etc/passwd"),
            Err(EndpointError::UnsupportedScheme)
        ));
        assert!(matches!(
            validate_webhook_url("https://user:password@example.com/webhook"),
            Err(EndpointError::CredentialsNotAllowed)
        ));
    }

    #[test]
    fn rejects_local_and_special_use_ipv4_endpoints() {
        for endpoint in [
            "http://0.0.0.0/webhook",
            "http://10.0.0.1/webhook",
            "http://100.64.0.1/webhook",
            "http://127.0.0.1/webhook",
            "http://127.1/webhook",
            "http://0177.0.0.1/webhook",
            "http://2130706433/webhook",
            "http://169.254.169.254/latest/meta-data",
            "http://172.16.0.1/webhook",
            "http://192.168.0.1/webhook",
            "http://198.18.0.1/webhook",
            "http://224.0.0.1/webhook",
        ] {
            assert!(
                matches!(
                    validate_webhook_url(endpoint),
                    Err(EndpointError::HostNotPublic(_))
                ),
                "{endpoint} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_local_and_special_use_ipv6_endpoints() {
        for endpoint in [
            "http://[::1]/webhook",
            "http://[::ffff:127.0.0.1]/webhook",
            "http://[fc00::1]/webhook",
            "http://[fe80::1]/webhook",
            "http://[2001:db8::1]/webhook",
            "http://[2002::1]/webhook",
        ] {
            assert!(
                matches!(
                    validate_webhook_url(endpoint),
                    Err(EndpointError::HostNotPublic(_))
                ),
                "{endpoint} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn rejects_localhost_before_delivery() {
        assert!(matches!(
            validate_webhook_endpoint("http://service.localhost/webhook").await,
            Err(EndpointError::HostNotPublic(_))
        ));
    }
}
