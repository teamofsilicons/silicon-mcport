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
    /// Cells' trusted runtime proxy resolves and pins public addresses. The
    /// isolated guest has no DNS or direct network access.
    CellsPublicInternet,
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

fn parse_endpoint(url: &str, policy: NetworkPolicy) -> Result<Url, McpError> {
    let url = Url::parse(url)
        .map_err(|_| McpError::new("invalid_url", "MCP endpoint must be a valid absolute URL."))?;
    if !matches!(url.scheme(), "http" | "https")
        || (policy != NetworkPolicy::LocalHost && url.scheme() != "https")
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
    if policy == NetworkPolicy::CellsPublicInternet {
        if url.port_or_known_default() != Some(443) {
            return Err(McpError::new(
                "unsafe_endpoint",
                "Cells cloud MCP endpoints must use HTTPS on port 443.",
            ));
        }
        let address = match url.host() {
            Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            _ => None,
        };
        if address.is_some_and(|ip| !public_address(ip)) {
            return Err(McpError::new(
                "unsafe_endpoint",
                "Cloud MCP endpoints cannot use private or reserved addresses.",
            ));
        }
    }
    Ok(url)
}

async fn resolve_endpoint(
    url: &str,
    policy: NetworkPolicy,
    timeout: Duration,
) -> Result<(Url, Vec<SocketAddr>), McpError> {
    let url = parse_endpoint(url, policy)?;
    if policy == NetworkPolicy::CellsPublicInternet {
        // DNS validation and address pinning happen in the trusted Cells host,
        // immediately before it opens the CONNECT destination. Guest DNS would
        // both fail in the sandbox and duplicate that security boundary.
        return Ok((url, vec![]));
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

/// Validate a URL with the same authority used when opening its transport.
pub async fn validate_url(url: &str, policy: NetworkPolicy) -> Result<(), McpError> {
    resolve_endpoint(url, policy, Duration::from_secs(10))
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
    if policy == NetworkPolicy::CellsPublicInternet {
        // This endpoint is part of Cells' service ABI, not a caller-provided
        // proxy or an inherited machine environment variable.
        builder = builder.proxy(
            reqwest::Proxy::https("http://127.0.0.1:18082").map_err(|_| {
                McpError::new(
                    "http_configuration",
                    "Could not configure the Cells network gateway.",
                )
            })?,
        );
    } else if let Some(Host::Domain(host)) = url.host() {
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

    #[tokio::test]
    async fn cells_uses_host_dns_and_keeps_url_guards() {
        // A deliberately nonexistent DNS name passes syntactic validation:
        // only the Cells proxy is allowed to resolve and connect it.
        validate_url(
            "https://mcport-fixture.invalid/mcp",
            NetworkPolicy::CellsPublicInternet,
        )
        .await
        .unwrap();
        for url in [
            "http://example.com/mcp",
            "https://127.0.0.1/mcp",
            "https://[::1]/mcp",
            "https://169.254.169.254/",
            "https://example.com:8443/mcp",
            "https://user:secret@example.com/mcp",
            "https://example.com/mcp#secret",
        ] {
            assert!(
                validate_url(url, NetworkPolicy::CellsPublicInternet)
                    .await
                    .is_err(),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn cells_transport_sends_connect_to_the_runtime_proxy() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:18082")
            .await
            .unwrap();
        let proxy = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
                assert!(request.len() < 8192);
            }
            socket
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8(request).unwrap()
        });
        let client = validated_http_client(
            "https://mcport-fixture.invalid/mcp",
            NetworkPolicy::CellsPublicInternet,
            Duration::from_secs(2),
            Duration::from_secs(3),
        )
        .await
        .unwrap();
        assert!(
            client
                .get("https://mcport-fixture.invalid/mcp")
                .send()
                .await
                .is_err()
        );
        let request = tokio::time::timeout(Duration::from_secs(3), proxy)
            .await
            .unwrap()
            .unwrap();
        assert!(request.starts_with("CONNECT mcport-fixture.invalid:443 HTTP/1.1\r\n"));
    }
}
