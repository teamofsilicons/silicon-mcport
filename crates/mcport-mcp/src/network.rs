use crate::McpError;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use url::{Host, Url};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Central executor: HTTPS and globally routable addresses only.
    PublicInternet,
    /// Explicitly registered endpoint on the local host connector.
    LocalHost,
}

fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !matches!(a, 0 | 10 | 127 | 224..=255)
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 169 && b == 254)
                && !(a == 172 && (16..=31).contains(&b))
                && !(a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                && !(a == 198 && ((18..=19).contains(&b) || (b == 51 && c == 100)))
                && !(a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            if let Some(ipv4) = ip.to_ipv4_mapped() {
                return public_address(IpAddr::V4(ipv4));
            }
            let s = ip.segments();
            (s[0] & 0xe000) == 0x2000
                && !(s[0] == 0x2001 && (s[1] == 0xdb8 || s[1] < 0x200))
                && !(s[0] == 0x3fff && s[1] & 0xf000 == 0)
                && s[0] != 0x2002
        }
    }
}

async fn resolve_endpoint(
    url: &str,
    policy: NetworkPolicy,
    timeout: Duration,
) -> Result<(Url, Vec<SocketAddr>), McpError> {
    let url = Url::parse(url)
        .map_err(|_| McpError::new("invalid_url", "MCP endpoint must be a valid absolute URL."))?;
    if !matches!(url.scheme(), "http" | "https")
        || (policy == NetworkPolicy::PublicInternet && url.scheme() != "https")
    {
        return Err(McpError::new(
            "unsafe_endpoint",
            "Cloud MCP endpoints must use HTTPS. Register private or local HTTP endpoints on a local host.",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(McpError::new(
            "unsafe_endpoint",
            "Endpoint URLs cannot contain user credentials or fragments. Configure authentication separately.",
        ));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| McpError::new("invalid_url", "MCP endpoint has no usable port."))?;
    let addresses = match url
        .host()
        .ok_or_else(|| McpError::new("invalid_url", "MCP endpoint has no hostname."))?
    {
        Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Host::Domain(host) => tokio::time::timeout(timeout, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| McpError::new("dns_timeout", "MCP endpoint DNS resolution timed out."))?
            .map_err(|_| {
                McpError::new("dns_failed", "MCP endpoint hostname could not be resolved.")
            })?
            .take(64)
            .collect(),
    };
    if addresses.is_empty() {
        return Err(McpError::new(
            "dns_failed",
            "MCP endpoint hostname has no address.",
        ));
    }
    if policy == NetworkPolicy::PublicInternet
        && addresses
            .iter()
            .any(|address| !public_address(address.ip()))
    {
        return Err(McpError::new(
            "unsafe_endpoint",
            "Cloud MCP endpoints cannot resolve to private, loopback, link-local or reserved addresses. Use an explicitly registered local host.",
        ));
    }
    Ok((url, addresses))
}

/// Validate without sending credentials or initiating an HTTP request.
pub async fn validate_public_url(url: &str) -> Result<(), McpError> {
    resolve_endpoint(url, NetworkPolicy::PublicInternet, Duration::from_secs(10))
        .await
        .map(|_| ())
}

/// A client pinned to the validated endpoint's DNS answers. Redirects and
/// environment proxies are disabled to avoid bypassing the address policy.
/// Use this client only for the URL whose hostname was validated here.
pub async fn validated_http_client(
    url: &str,
    policy: NetworkPolicy,
    connect_timeout: Duration,
    timeout: Duration,
) -> Result<reqwest::Client, McpError> {
    let (url, addresses) = resolve_endpoint(url, policy, connect_timeout).await?;
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .pool_max_idle_per_host(0);
    if let Some(Host::Domain(host)) = url.host() {
        builder = builder.resolve_to_addrs(host, &addresses);
    }
    builder.build().map_err(|_| {
        McpError::new(
            "http_configuration",
            "Could not configure the MCP HTTP client.",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_reserved_and_mapped_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "172.31.0.1",
            "192.168.1.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::",
        ] {
            assert!(!public_address(address.parse().unwrap()), "{address}");
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(public_address(address.parse().unwrap()), "{address}");
        }
    }
    #[tokio::test]
    async fn public_policy_rejects_private_urls_without_sending_requests() {
        for url in [
            "http://example.com/mcp",
            "https://127.0.0.1/mcp",
            "https://[::1]/mcp",
            "https://user:secret@example.com/mcp",
            "https://example.com/mcp#secret",
        ] {
            assert!(validate_public_url(url).await.is_err(), "{url}");
        }
    }
}
