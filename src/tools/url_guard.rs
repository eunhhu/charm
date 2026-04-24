//! SSRF protection for outbound HTTP requests.
//!
//! Validates URLs before dispatch to block requests targeting internal/private
//! networks. Blocks: non-http/https schemes, localhost, loopback, private,
//! link-local, unique-local (IPv6), and unspecified addresses.
//!
//! Also provides UTF-8-safe string slicing helpers used by the web tools.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

#[derive(Debug, Clone)]
pub struct ValidatedUrl {
    host: String,
    resolved_addrs: Vec<SocketAddr>,
}

impl ValidatedUrl {
    pub fn pin_dns(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        if self.resolved_addrs.is_empty() {
            builder
        } else {
            builder.resolve_to_addrs(&self.host, &self.resolved_addrs)
        }
    }
}

// ---------------------------------------------------------------------------
// UTF-8-safe string slicing helpers
// ---------------------------------------------------------------------------

/// Truncate `s` to at most `max_bytes` bytes, landing on a valid char boundary.
///
/// Returns a prefix of `s` that is no longer than `max_bytes` bytes and is
/// guaranteed to be valid UTF-8 (i.e. does not split a multi-byte character).
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Slice `s[start..end]` safely, clamping both indices to the nearest char
/// boundaries. Returns `""` when `start` is past the end of the string.
pub fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    if start >= s.len() {
        return "";
    }
    let start = next_char_boundary(s, start);
    let end = next_char_boundary(s, end.min(s.len()));
    if start >= end {
        return "";
    }
    &s[start..end]
}

/// Walk backwards from `byte_idx` to the nearest char boundary at or before it.
fn next_char_boundary(s: &str, byte_idx: usize) -> usize {
    if byte_idx >= s.len() {
        return s.len();
    }
    let mut idx = byte_idx;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Validate a URL against SSRF protections.
///
/// Returns `Ok(())` if the URL is safe to fetch, or `Err` with a description
/// of why it was blocked.
pub fn validate_url(url_str: &str) -> anyhow::Result<ValidatedUrl> {
    let parsed = validate_url_shape(url_str)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Blocked: URL has no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();

    let mut resolved_addrs = Vec::new();
    if host.parse::<IpAddr>().is_err() {
        let port = parsed.port_or_known_default().unwrap_or(80);
        resolved_addrs = resolve_host_addrs(&host, port)?;
        validate_resolved_addrs(&resolved_addrs)?;
    }

    Ok(ValidatedUrl {
        host,
        resolved_addrs,
    })
}

fn validate_url_shape(url_str: &str) -> anyhow::Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url_str).map_err(|e| anyhow::anyhow!("Invalid URL: {}", e))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(anyhow::anyhow!(
                "Blocked: unsupported scheme '{}'. Only http and https are allowed.",
                other
            ));
        }
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Blocked: URL has no host"))?;

    if host.is_empty() {
        return Err(anyhow::anyhow!("Blocked: URL has empty host"));
    }

    // Strip surrounding brackets that may appear for IPv6 literals.
    let host = host.trim_start_matches('[').trim_end_matches(']');

    if host.eq_ignore_ascii_case("localhost") {
        return Err(anyhow::anyhow!("Blocked: localhost is not allowed"));
    }
    if host.ends_with(".local") || host.ends_with(".localhost") {
        return Err(anyhow::anyhow!(
            "Blocked: internal hostname '{}' is not allowed",
            host
        ));
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        validate_ip(&ip)?;
    }

    Ok(parsed)
}

#[cfg(test)]
fn validate_url_with_resolved_ips(url_str: &str, resolved_ips: &[IpAddr]) -> anyhow::Result<()> {
    validate_url_shape(url_str)?;
    for ip in resolved_ips {
        validate_ip(ip)?;
    }
    Ok(())
}

fn validate_resolved_addrs(addrs: &[SocketAddr]) -> anyhow::Result<()> {
    for addr in addrs {
        validate_ip(&addr.ip())?;
    }
    Ok(())
}

fn resolve_host_addrs(host: &str, port: u16) -> anyhow::Result<Vec<SocketAddr>> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("Blocked: failed to resolve host '{}': {}", host, e))?;
    let mut resolved = Vec::new();
    for addr in addrs {
        if !resolved.contains(&addr) {
            resolved.push(addr);
        }
    }
    if resolved.is_empty() {
        return Err(anyhow::anyhow!(
            "Blocked: host '{}' resolved to no addresses",
            host
        ));
    }
    Ok(resolved)
}

fn validate_ip(ip: &IpAddr) -> anyhow::Result<()> {
    match ip {
        IpAddr::V4(v4) => validate_ipv4(v4),
        IpAddr::V6(v6) => validate_ipv6(v6),
    }
}

fn validate_ipv4(ip: &Ipv4Addr) -> anyhow::Result<()> {
    if ip.is_loopback() {
        return Err(anyhow::anyhow!(
            "Blocked: loopback address {} is not allowed",
            ip
        ));
    }
    if ip.is_private() {
        return Err(anyhow::anyhow!(
            "Blocked: private address {} is not allowed",
            ip
        ));
    }
    if ip.is_link_local() {
        return Err(anyhow::anyhow!(
            "Blocked: link-local address {} is not allowed",
            ip
        ));
    }
    if ip.is_unspecified() {
        return Err(anyhow::anyhow!(
            "Blocked: unspecified address {} is not allowed",
            ip
        ));
    }
    Ok(())
}

fn validate_ipv6(ip: &Ipv6Addr) -> anyhow::Result<()> {
    if ip.is_loopback() {
        return Err(anyhow::anyhow!(
            "Blocked: loopback address {} is not allowed",
            ip
        ));
    }
    if ip.is_unspecified() {
        return Err(anyhow::anyhow!(
            "Blocked: unspecified address {} is not allowed",
            ip
        ));
    }
    let segments = ip.segments();
    // Link-local: fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return Err(anyhow::anyhow!(
            "Blocked: link-local address {} is not allowed",
            ip
        ));
    }
    // Unique-local: fc00::/7 (covers fc00::/8 and fd00::/8)
    if (segments[0] & 0xfe00) == 0xfc00 {
        return Err(anyhow::anyhow!(
            "Blocked: unique-local address {} is not allowed",
            ip
        ));
    }
    // IPv4-mapped addresses (::ffff:x.x.x.x) – validate the embedded IPv4.
    if let Some(v4) = ip.to_ipv4_mapped() {
        validate_ipv4(&v4)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Allowed URLs ---

    #[test]
    fn allows_https_external() {
        assert!(validate_url("https://example.com/path").is_ok());
        assert!(validate_url("http://example.com/").is_ok());
        assert!(validate_url("https://api.github.com/repos").is_ok());
    }

    #[test]
    fn allows_external_ip() {
        assert!(validate_url("http://93.184.216.34/").is_ok()); // example.com IP
    }

    // --- Blocked: non-http schemes ---

    #[test]
    fn blocks_file_scheme() {
        assert!(validate_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn blocks_ftp_scheme() {
        assert!(validate_url("ftp://example.com/").is_err());
    }

    #[test]
    fn blocks_data_scheme() {
        assert!(validate_url("data:text/html,<h1>hi</h1>").is_err());
    }

    #[test]
    fn blocks_javascript_scheme() {
        assert!(validate_url("javascript:alert(1)").is_err());
    }

    // --- Blocked: localhost ---

    #[test]
    fn blocks_localhost() {
        assert!(validate_url("http://localhost/").is_err());
        assert!(validate_url("http://localhost:8080/").is_err());
        assert!(validate_url("https://LOCALHOST/").is_err());
    }

    // --- Blocked: loopback ---

    #[test]
    fn blocks_loopback_ipv4() {
        assert!(validate_url("http://127.0.0.1/").is_err());
        assert!(validate_url("http://127.0.0.1:8080/").is_err());
        assert!(validate_url("http://127.255.255.255/").is_err());
    }

    #[test]
    fn blocks_loopback_ipv6() {
        assert!(validate_url("http://[::1]/").is_err());
    }

    // --- Blocked: private ---

    #[test]
    fn blocks_private_10() {
        assert!(validate_url("http://10.0.0.1/").is_err());
        assert!(validate_url("http://10.255.255.255/").is_err());
    }

    #[test]
    fn blocks_private_172() {
        assert!(validate_url("http://172.16.0.1/").is_err());
        assert!(validate_url("http://172.31.255.255/").is_err());
    }

    #[test]
    fn blocks_private_192() {
        assert!(validate_url("http://192.168.1.1/").is_err());
        assert!(validate_url("http://192.168.0.0/").is_err());
    }

    // --- Blocked: link-local ---

    #[test]
    fn blocks_link_local_ipv4() {
        assert!(validate_url("http://169.254.1.1/").is_err());
    }

    #[test]
    fn blocks_link_local_ipv6() {
        assert!(validate_url("http://[fe80::1]/").is_err());
    }

    // --- Blocked: unspecified ---

    #[test]
    fn blocks_unspecified_ipv4() {
        assert!(validate_url("http://0.0.0.0/").is_err());
    }

    #[test]
    fn blocks_unspecified_ipv6() {
        assert!(validate_url("http://[::]/").is_err());
    }

    // --- Blocked: internal hostnames ---

    #[test]
    fn blocks_dot_local() {
        assert!(validate_url("http://myhost.local/").is_err());
    }

    #[test]
    fn blocks_dot_localhost() {
        assert!(validate_url("http://myhost.localhost/").is_err());
    }

    #[test]
    fn blocks_hostname_that_resolves_to_private_ip() {
        let resolved_ips = [IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))];
        assert!(validate_url_with_resolved_ips("http://internal.example/", &resolved_ips).is_err());
    }

    #[test]
    fn allows_hostname_that_resolves_to_external_ip() {
        let resolved_ips = [IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))];
        assert!(validate_url_with_resolved_ips("http://example.com/", &resolved_ips).is_ok());
    }

    // --- Blocked: IPv4-mapped IPv6 ---

    #[test]
    fn blocks_ipv4_mapped_loopback() {
        assert!(validate_url("http://[::ffff:127.0.0.1]/").is_err());
    }

    #[test]
    fn blocks_ipv4_mapped_private() {
        assert!(validate_url("http://[::ffff:192.168.1.1]/").is_err());
    }

    // --- Blocked: IPv6 unique-local (fc00::/7) ---

    #[test]
    fn blocks_ipv6_unique_local_fc00() {
        assert!(validate_url("http://[fc00::1]/").is_err());
        assert!(validate_url("http://[fc00::]/").is_err());
        assert!(validate_url("http://[fc00:1234:5678::1]/").is_err());
    }

    #[test]
    fn blocks_ipv6_unique_local_fd00() {
        assert!(validate_url("http://[fd00::1]/").is_err());
        assert!(validate_url("http://[fd00:dead:beef::1]/").is_err());
    }

    // --- UTF-8-safe truncation helpers ---

    #[test]
    fn truncate_str_ascii() {
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_multibyte() {
        // "한글" is 6 bytes (3 bytes per char)
        let s = "한글abc";
        assert_eq!(truncate_str(s, 6), "한글");
        // Cutting at byte 4 would split '한' — must back up to byte 3
        assert_eq!(truncate_str(s, 4), "한");
        assert_eq!(truncate_str(s, 3), "한");
    }

    #[test]
    fn safe_slice_basic() {
        let s = "hello world";
        assert_eq!(safe_slice(s, 0, 5), "hello");
        assert_eq!(safe_slice(s, 6, 11), "world");
    }

    #[test]
    fn safe_slice_multibyte() {
        let s = "한글abc";
        // Slice from byte 3 (start of '글') to byte 9 (end of string)
        assert_eq!(safe_slice(s, 3, 9), "글abc");
        // Start at a mid-char boundary — clamp to char boundary
        assert_eq!(safe_slice(s, 4, 9), "글abc");
    }

    #[test]
    fn safe_slice_out_of_bounds() {
        let s = "abc";
        assert_eq!(safe_slice(s, 10, 20), "");
        assert_eq!(safe_slice(s, 0, 100), "abc");
    }
}
