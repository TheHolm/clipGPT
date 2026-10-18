//! HTTP transport to the LLM API: URL/scheme checks, the redirect policy,
//! and size-capped response reading.
//!
//! Rules (see README.md "Security"):
//! * only `http` and `https` URLs are accepted
//! * plain `http` to loopback or to one of this computer's own addresses is
//!   silent; plain `http` to any other host prints a warning on stderr (the
//!   request still goes ahead)
//! * redirects are refused unless `allow_redirects` is set; when allowed,
//!   every hop gets the same checks, up to [`MAX_REDIRECTS`] hops
//! * responses are capped at [`MAX_RESPONSE_BYTES`]
//! * the whole request is bounded by the configured timeout (if any); a
//!   timeout is reported as such, with the limit and how to raise it
//! * error messages never contain the response body

use reqwest::blocking::Client;
use reqwest::{redirect, Url};
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

/// Maximum number of redirect hops followed when redirects are allowed
/// (the same limit reqwest uses by default).
pub const MAX_REDIRECTS: usize = 10;

/// Maximum accepted response body size.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// How a URL's traffic travels.
#[derive(Debug, PartialEq, Eq)]
pub enum Transport {
    /// https.
    Encrypted,
    /// http to loopback or to an address of this computer.
    PlainLocal,
    /// http to any other host - worth a warning.
    PlainRemote,
}

/// Classify `url`. `is_local` decides whether a host (as written in the
/// URL, IPv6 without brackets) and port belong to this computer. Schemes
/// other than http/https are an error.
pub fn assess_url(url: &Url, is_local: &dyn Fn(&str, u16) -> bool) -> Result<Transport, String> {
    match url.scheme() {
        "https" => Ok(Transport::Encrypted),
        "http" => {
            let host = url
                .host_str()
                .ok_or_else(|| format!("URL has no host: {}", url_origin(url)))?;
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let port = url.port_or_known_default().unwrap_or(80);
            if is_local(host, port) {
                Ok(Transport::PlainLocal)
            } else {
                Ok(Transport::PlainRemote)
            }
        }
        other => Err(format!(
            "Unsupported URL scheme '{}': only http and https are allowed",
            other
        )),
    }
}

/// Scheme, host and port of `url` only - used in messages so a path or
/// query string is never echoed.
fn url_origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Warning text for plain http to a host that is not this computer.
pub fn insecure_warning(url: &Url) -> String {
    format!(
        "Warning: {} uses unencrypted http to a host that is not this computer; \
         the API key and clipboard text travel in clear text",
        url_origin(url)
    )
}

/// True if `ip` is loopback, unspecified (which connects to this host), or
/// assigned to one of this computer's interfaces. The last check binds a
/// UDP socket to the address: the OS only allows that for its own
/// addresses. Multicast and broadcast are never treated as local, since
/// binding to them can succeed without them being "this computer".
pub fn ip_is_local(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    if ip.is_multicast() {
        return false;
    }
    if let IpAddr::V4(v4) = ip {
        if v4.is_broadcast() {
            return false;
        }
    }
    UdpSocket::bind(SocketAddr::new(ip, 0)).is_ok()
}

/// True if `host` (an IP literal or a name) belongs to this computer. A
/// name must resolve, and every address it resolves to must be local;
/// unresolvable names count as not local (so they get the warning - the
/// request itself will then fail with its own error).
pub fn host_is_local(host: &str, port: u16) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip_is_local(ip);
    }
    match (host, port).to_socket_addrs() {
        Ok(addrs) => {
            let addrs: Vec<SocketAddr> = addrs.collect();
            !addrs.is_empty() && addrs.iter().all(|a| ip_is_local(a.ip()))
        }
        Err(_) => false,
    }
}

/// Check `url` before the request is sent: errors for bad schemes, and
/// returns the warning to print (if any) for plain http to another host.
pub fn check_url(
    url: &Url,
    is_local: &dyn Fn(&str, u16) -> bool,
) -> Result<Option<String>, String> {
    match assess_url(url, is_local)? {
        Transport::PlainRemote => Ok(Some(insecure_warning(url))),
        Transport::Encrypted | Transport::PlainLocal => Ok(None),
    }
}

/// Decide one redirect hop when redirects are allowed. `visited` is the
/// number of URLs already requested (reqwest's `previous().len()`). Returns
/// a warning to print for plain http to another host, or an error to stop.
pub fn redirect_decision(
    next: &Url,
    visited: usize,
    is_local: &dyn Fn(&str, u16) -> bool,
) -> Result<Option<String>, String> {
    if visited > MAX_REDIRECTS {
        return Err(format!("Too many redirects (more than {})", MAX_REDIRECTS));
    }
    check_url(next, is_local).map_err(|e| format!("Redirect refused: {}", e))
}

/// Build the HTTP client. With `allow_redirects` false, no redirect is
/// followed (a 3xx then becomes an error in [`post_json`]); otherwise each
/// hop goes through [`redirect_decision`]. reqwest itself drops the
/// Authorization header when a redirect changes host, port or scheme.
/// `timeout` bounds the whole request, response body included; `None`
/// means no limit.
pub fn build_client(allow_redirects: bool, timeout: Option<Duration>) -> Result<Client, String> {
    let policy = if allow_redirects {
        redirect::Policy::custom(|attempt| {
            match redirect_decision(attempt.url(), attempt.previous().len(), &host_is_local) {
                Ok(warning) => {
                    if let Some(warning) = warning {
                        eprintln!("{}", warning);
                    }
                    attempt.follow()
                }
                Err(e) => attempt.error(e),
            }
        })
    } else {
        redirect::Policy::none()
    };
    Client::builder()
        .redirect(policy)
        .timeout(timeout)
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", error_chain(&e)))
}

/// Read at most `limit` bytes from `reader`; more than that is an error.
/// A read that hit the request `timeout` is reported by [`timeout_error`].
pub fn read_capped(
    reader: impl Read,
    limit: usize,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| {
            if io_is_timeout(&e) {
                timeout_error(timeout, &e)
            } else {
                format!("Failed to read response: {}", e)
            }
        })?;
    if body.len() > limit {
        return Err(format!(
            "Response is larger than {} bytes; discarded",
            limit
        ));
    }
    Ok(body)
}

/// Whether a body read failed because the request timed out: reqwest
/// wraps its own timeout error in the `io::Error`; a plain `TimedOut` kind
/// counts too.
fn io_is_timeout(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::TimedOut
        || err
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
            .is_some_and(reqwest::Error::is_timeout)
}

/// Message for a request that timed out. With a configured limit it names
/// the limit and how to change it; without one (`None`, so the timeout
/// came from the network or OS) it gives the underlying error.
pub fn timeout_error(timeout: Option<Duration>, err: &dyn std::error::Error) -> String {
    match timeout {
        Some(limit) => format!(
            "The LLM did not answer within {} s; raise request_timeout in config.ini or use \
             --timeout SECONDS (0 = no limit)",
            limit.as_secs()
        ),
        None => format!("Request timed out: {}", error_chain(err)),
    }
}

/// Format an error with its whole `source()` chain, so the reason behind a
/// reqwest error (e.g. a refused redirect) is visible.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        message.push_str(": ");
        message.push_str(&inner.to_string());
        source = inner.source();
    }
    message
}

/// POST `body` as JSON to `url` and return the response body as text.
/// Non-2xx statuses are errors that report only the status code - never
/// the body, which may echo the clipboard text back.
pub fn post_json<T: serde::Serialize>(
    client: &Client,
    url: &Url,
    api_key: Option<&str>,
    body: &T,
    timeout: Option<Duration>,
) -> Result<String, String> {
    let mut request = client.post(url.clone()).json(body);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().map_err(|e| {
        if e.is_timeout() {
            timeout_error(timeout, &e)
        } else {
            format!("Request failed: {}", error_chain(&e))
        }
    })?;

    let status = response.status();
    if status.is_redirection() {
        return Err(format!(
            "API returned redirect (HTTP {}); redirects are disabled - fix api_url or set allow_redirects=true",
            status.as_u16()
        ));
    }
    if !status.is_success() {
        return Err(format!("API returned HTTP {}", status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
    {
        return Err(format!(
            "Response is larger than {} bytes; discarded",
            MAX_RESPONSE_BYTES
        ));
    }
    let bytes = read_capped(response, MAX_RESPONSE_BYTES, timeout)?;
    String::from_utf8(bytes).map_err(|_| "Response is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// Fake locality check: only "127.0.0.1", "::1", "localhost" and
    /// "192.168.1.20" (pretend LAN address of this machine) are local.
    fn fake_local(host: &str, _port: u16) -> bool {
        matches!(host, "127.0.0.1" | "::1" | "localhost" | "192.168.1.20")
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn https_is_encrypted() {
        let u = url("https://api.openai.com/v1/chat/completions");
        assert_eq!(assess_url(&u, &fake_local), Ok(Transport::Encrypted));
        assert_eq!(check_url(&u, &fake_local), Ok(None));
    }

    #[test]
    fn http_to_loopback_and_own_lan_ip_is_silent() {
        for s in [
            "http://127.0.0.1:11434/v1/chat/completions",
            "http://localhost:11434/",
            "http://[::1]:11434/",
            "http://192.168.1.20:11434/",
        ] {
            assert_eq!(
                assess_url(&url(s), &fake_local),
                Ok(Transport::PlainLocal),
                "{s}"
            );
            assert_eq!(check_url(&url(s), &fake_local), Ok(None), "{s}");
        }
    }

    #[test]
    fn http_to_other_host_warns_without_path() {
        let u = url("http://192.168.1.50:11434/v1/chat?token=abc");
        assert_eq!(assess_url(&u, &fake_local), Ok(Transport::PlainRemote));
        let warning = check_url(&u, &fake_local).unwrap().unwrap();
        assert!(warning.contains("http://192.168.1.50:11434"));
        assert!(!warning.contains("token"));
    }

    #[test]
    fn other_schemes_rejected() {
        for s in [
            "ftp://example.com/",
            "file:///etc/passwd",
            "ws://localhost/",
        ] {
            assert!(assess_url(&url(s), &fake_local).is_err(), "{s}");
        }
    }

    #[test]
    fn redirect_hops_are_checked() {
        assert_eq!(
            redirect_decision(&url("https://a.example/"), 1, &fake_local),
            Ok(None)
        );
        assert!(redirect_decision(&url("http://a.example/"), 1, &fake_local)
            .unwrap()
            .is_some());
        let err = redirect_decision(&url("file:///etc/passwd"), 1, &fake_local).unwrap_err();
        assert!(err.contains("Redirect refused"));
    }

    #[test]
    fn redirect_limit_enforced() {
        let u = url("https://a.example/");
        assert!(redirect_decision(&u, MAX_REDIRECTS, &fake_local).is_ok());
        assert!(redirect_decision(&u, MAX_REDIRECTS + 1, &fake_local).is_err());
    }

    #[test]
    fn loopback_and_unspecified_are_local() {
        assert!(ip_is_local(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(ip_is_local(IpAddr::V4(Ipv4Addr::new(127, 1, 2, 3))));
        assert!(ip_is_local(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(ip_is_local(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        // IPv4-mapped loopback.
        assert!(ip_is_local("::ffff:127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn foreign_multicast_and_broadcast_are_not_local() {
        // TEST-NET-3 (RFC 5737) is never assigned to a real interface.
        assert!(!ip_is_local(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))));
        assert!(!ip_is_local(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1))));
        assert!(!ip_is_local(IpAddr::V4(Ipv4Addr::BROADCAST)));
        // Documentation prefix (RFC 3849).
        assert!(!ip_is_local("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn host_is_local_handles_literals_and_names() {
        assert!(host_is_local("127.0.0.1", 80));
        assert!(host_is_local("::1", 80));
        assert!(host_is_local("localhost", 80));
        assert!(!host_is_local("203.0.113.7", 80));
        // .invalid is reserved (RFC 6761) and never resolves.
        assert!(!host_is_local("nothing.invalid", 80));
    }

    #[test]
    fn real_check_url_on_loopback_is_silent() {
        let u = url("http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(check_url(&u, &host_is_local), Ok(None));
    }

    #[test]
    fn read_capped_enforces_limit() {
        assert_eq!(read_capped(&b"12345"[..], 5, None), Ok(b"12345".to_vec()));
        let err = read_capped(&b"123456"[..], 5, None).unwrap_err();
        assert!(!err.contains("123456"));
    }

    /// Serve `responses` (one per connection) on a loopback port and return
    /// the base URL plus a handle yielding every raw request received.
    fn serve(responses: Vec<String>) -> (Url, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = url(&format!("http://{}/", listener.local_addr().unwrap()));
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                // Read headers, then the body by Content-Length.
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + len || n == 0 {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&raw).to_string());
                stream.write_all(response.as_bytes()).unwrap();
            }
            requests
        });
        (base, handle)
    }

    /// Minimal HTTP/1.1 response with `status` line and `body`.
    fn http_response(status: &str, extra_headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn post_json_success_sends_key_only_when_given() {
        let (base, handle) = serve(vec![
            http_response("200 OK", "", "{\"ok\":1}"),
            http_response("200 OK", "", "{\"ok\":2}"),
        ]);
        let client = build_client(false, None).unwrap();
        let body = serde_json::json!({"x": 1});
        assert_eq!(
            post_json(&client, &base, Some("sk-test"), &body, None),
            Ok("{\"ok\":1}".to_string())
        );
        assert_eq!(
            post_json(&client, &base, None, &body, None),
            Ok("{\"ok\":2}".to_string())
        );
        let requests = handle.join().unwrap();
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-test"));
        assert!(!requests[1].to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn post_json_error_status_hides_body() {
        let (base, handle) = serve(vec![http_response(
            "400 Bad Request",
            "",
            "{\"error\":\"your secret clipboard text\"}",
        )]);
        let client = build_client(false, None).unwrap();
        let err = post_json(&client, &base, None, &serde_json::json!({}), None).unwrap_err();
        handle.join().unwrap();
        assert!(err.contains("400"));
        assert!(!err.contains("secret"));
    }

    #[test]
    fn post_json_refuses_redirect_by_default() {
        let (base, handle) = serve(vec![http_response(
            "307 Temporary Redirect",
            "Location: http://203.0.113.7/steal\r\n",
            "",
        )]);
        let client = build_client(false, None).unwrap();
        let err = post_json(
            &client,
            &base,
            Some("sk-test"),
            &serde_json::json!({}),
            None,
        )
        .unwrap_err();
        assert_eq!(handle.join().unwrap().len(), 1);
        assert!(err.contains("307"));
        assert!(err.contains("allow_redirects"));
    }

    #[test]
    fn post_json_follows_allowed_redirect() {
        let (target, target_handle) = serve(vec![http_response("200 OK", "", "done")]);
        let (base, handle) = serve(vec![http_response(
            "307 Temporary Redirect",
            &format!("Location: {}final\r\n", target),
            "",
        )]);
        let client = build_client(true, None).unwrap();
        let result = post_json(&client, &base, None, &serde_json::json!({}), None);
        handle.join().unwrap();
        let requests = target_handle.join().unwrap();
        assert_eq!(result, Ok("done".to_string()));
        assert!(requests[0].starts_with("POST /final"));
    }

    #[test]
    fn post_json_allowed_redirect_still_refuses_bad_scheme() {
        let (base, handle) = serve(vec![http_response(
            "307 Temporary Redirect",
            "Location: ftp://203.0.113.7/x\r\n",
            "",
        )]);
        let client = build_client(true, None).unwrap();
        let err = post_json(&client, &base, None, &serde_json::json!({}), None).unwrap_err();
        handle.join().unwrap();
        assert!(err.contains("Redirect refused"), "{err}");
    }

    #[test]
    fn post_json_rejects_oversized_response() {
        let big = "a".repeat(MAX_RESPONSE_BYTES + 1);
        let (base, handle) = serve(vec![http_response("200 OK", "", &big)]);
        let client = build_client(false, None).unwrap();
        let err = post_json(&client, &base, None, &serde_json::json!({}), None).unwrap_err();
        let _ = handle.join();
        assert!(err.contains("larger than"));
    }

    /// A server for one request that reads it, then after `delay` writes
    /// `response` and keeps the connection open for `hold` more (so a
    /// partial response stalls instead of ending).
    fn serve_slow(delay: Duration, response: String, hold: Duration) -> Url {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            std::thread::sleep(delay);
            let _ = stream.write_all(response.as_bytes());
            std::thread::sleep(hold);
        });
        base
    }

    #[test]
    fn post_json_times_out_waiting_for_answer() {
        let base = serve_slow(
            Duration::from_secs(5),
            http_response("200 OK", "", "late"),
            Duration::ZERO,
        );
        let limit = Some(Duration::from_secs(1));
        let client = build_client(false, limit).unwrap();
        let start = std::time::Instant::now();
        let err = post_json(&client, &base, None, &serde_json::json!({}), limit).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(err.contains("did not answer within 1 s"), "{err}");
        assert!(
            err.contains("request_timeout") && err.contains("--timeout"),
            "{err}"
        );
    }

    #[test]
    fn post_json_times_out_reading_body() {
        // Headers and part of the body arrive, then the server stalls.
        let partial = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{\"choices\"".to_string();
        let base = serve_slow(Duration::ZERO, partial, Duration::from_secs(5));
        let limit = Some(Duration::from_secs(1));
        let client = build_client(false, limit).unwrap();
        let err = post_json(&client, &base, None, &serde_json::json!({}), limit).unwrap_err();
        assert!(err.contains("did not answer within 1 s"), "{err}");
    }

    #[test]
    fn no_limit_waits_for_slow_answer() {
        // With no timeout a slow answer still arrives (kept short here).
        let base = serve_slow(
            Duration::from_millis(1500),
            http_response("200 OK", "", "slow but fine"),
            Duration::ZERO,
        );
        let client = build_client(false, None).unwrap();
        let body = post_json(&client, &base, None, &serde_json::json!({}), None);
        assert_eq!(body, Ok("slow but fine".to_string()));
    }

    #[test]
    fn timeout_error_without_limit_shows_cause() {
        let cause = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let msg = timeout_error(None, &cause);
        assert!(msg.starts_with("Request timed out: "), "{msg}");
        let msg = timeout_error(Some(Duration::from_secs(90)), &cause);
        assert!(msg.contains("within 90 s"), "{msg}");
    }

    #[test]
    fn io_timeout_detection() {
        assert!(io_is_timeout(&std::io::Error::from(
            std::io::ErrorKind::TimedOut
        )));
        assert!(!io_is_timeout(&std::io::Error::other("reset")));
        /// A reader that times out.
        struct Stalled;
        impl Read for Stalled {
            /// Always times out.
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::TimedOut))
            }
        }
        let err = read_capped(Stalled, 5, Some(Duration::from_secs(3))).unwrap_err();
        assert!(err.contains("within 3 s"), "{err}");
    }

    #[test]
    fn build_client_both_modes() {
        assert!(build_client(false, None).is_ok());
        assert!(build_client(true, Some(Duration::from_secs(5))).is_ok());
    }

    #[test]
    fn error_chain_includes_sources() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let e = Outer(std::io::Error::other("inner"));
        assert_eq!(error_chain(&e), "outer: inner");
    }
}
