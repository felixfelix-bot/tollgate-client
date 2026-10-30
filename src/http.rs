//! Minimal plain-HTTP client — std only, no TLS.
//!
//! The v1 TollGate serves its advert and takes payment on **plain HTTP :2121**
//! (`src/main.go:463` binds HTTP, not HTTPS), so the gate leg needs no TLS at all.
//! Mints are a different story (they are HTTPS); that is the wallet's problem, not
//! this transport's, which is exactly why the wallet is a trait.
//!
//! Deliberately dumb: connect, one request, read to EOF. The gate is a LAN peer.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    timeout_ms: u64,
) -> Result<(u16, String), String> {
    request_with_headers(method, url, body, timeout_ms, &[("Content-Type", "text/plain")])
}

pub fn request_with_headers(
    method: &str,
    url: &str,
    body: Option<&str>,
    timeout_ms: u64,
    headers: &[(&str, &str)],
) -> Result<(u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("only plain http:// is supported, got {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:80")
    };

    let mut stream = TcpStream::connect(&addr).map_err(|e| format!("connect {addr}: {e}"))?;
    let t = Some(Duration::from_millis(timeout_ms));
    stream.set_read_timeout(t).map_err(|e| e.to_string())?;
    stream.set_write_timeout(t).map_err(|e| e.to_string())?;

    let body = body.unwrap_or("");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if method != "GET" {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write {addr}: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read {addr}: {e}"))?;
    let text = String::from_utf8_lossy(&raw).to_string();

    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| "malformed HTTP response (no status line)".to_string())?;
    let body = match text.find("\r\n\r\n") {
        Some(i) => text[i + 4..].to_string(),
        None => String::new(),
    };
    Ok((status, body))
}
