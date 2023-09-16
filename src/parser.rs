//! Incremental, I/O-free HTTP/1.1 request parser shared by both servers.
//!
//! Callers append bytes to a buffer and call [`parse_request`]. It returns
//! `Ok(None)` while the request is incomplete, or the request and the number
//! of bytes it used (so pipelined requests can follow in the same buffer).

use std::fmt;

pub const MAX_HEAD: usize = 16 * 1024;
pub const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Version {
    Http10,
    Http11,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub version: Version,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// Case-insensitive header lookup (first match).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// Whether the connection should stay open after this request.
    pub fn keep_alive(&self) -> bool {
        let conn = self.header("connection").map(|v| v.to_ascii_lowercase());
        let has = |tok: &str| conn.as_deref().is_some_and(|c| c.split(',').any(|t| t.trim() == tok));
        match self.version {
            Version::Http11 => !has("close"),
            Version::Http10 => has("keep-alive"),
        }
    }

    /// Path part of the target, without the query string.
    pub fn path(&self) -> &str {
        self.target.split(['?', '#']).next().unwrap_or("/")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    BadRequestLine,
    BadVersion,
    BadHeader,
    HeadTooLarge,
    BodyTooLarge,
    BadContentLength,
    BadChunk,
    UnsupportedTransferEncoding,
}

impl ParseError {
    /// The status code a server should answer with.
    pub fn status(&self) -> u16 {
        match self {
            ParseError::HeadTooLarge => 431,
            ParseError::BodyTooLarge => 413,
            ParseError::BadVersion => 505,
            ParseError::UnsupportedTransferEncoding => 501,
            _ => 400,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ParseError {}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Parse one request from the front of `buf`.
pub fn parse_request(buf: &[u8]) -> Result<Option<(Request, usize)>, ParseError> {
    // Tolerate stray CRLFs between pipelined requests (RFC 9112 §2.2).
    let mut start = 0;
    while buf[start..].starts_with(b"\r\n") {
        start += 2;
    }
    let buf_rest = &buf[start..];
    let Some(head_end) = find(buf_rest, b"\r\n\r\n") else {
        if buf_rest.len() > MAX_HEAD {
            return Err(ParseError::HeadTooLarge);
        }
        return Ok(None);
    };
    if head_end > MAX_HEAD {
        return Err(ParseError::HeadTooLarge);
    }
    let head = std::str::from_utf8(&buf_rest[..head_end]).map_err(|_| ParseError::BadHeader)?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(ParseError::BadRequestLine)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ParseError::BadRequestLine);
    };
    if !is_token(method) || target.is_empty() {
        return Err(ParseError::BadRequestLine);
    }
    let version = match version {
        "HTTP/1.1" => Version::Http11,
        "HTTP/1.0" => Version::Http10,
        v if v.starts_with("HTTP/") => return Err(ParseError::BadVersion),
        _ => return Err(ParseError::BadRequestLine),
    };

    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(ParseError::BadHeader)?;
        if !is_token(name) {
            // Also rejects whitespace before the colon and obsolete line folding.
            return Err(ParseError::BadHeader);
        }
        headers.push((name.to_string(), value.trim_matches([' ', '\t']).to_string()));
    }
    let mut req = Request { method: method.to_string(), target: target.to_string(), version, headers, body: Vec::new() };
    let body_start = start + head_end + 4;
    let body_buf = &buf[body_start..];

    if let Some(te) = req.header("transfer-encoding") {
        if !te.trim().eq_ignore_ascii_case("chunked") {
            return Err(ParseError::UnsupportedTransferEncoding);
        }
        if req.header("content-length").is_some() {
            // Request smuggling guard: never accept both.
            return Err(ParseError::BadContentLength);
        }
        return Ok(decode_chunked(body_buf)?.map(|(body, used)| {
            req.body = body;
            (req, body_start + used)
        }));
    }

    let lengths: Vec<&str> =
        req.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.as_str()).collect();
    let len = match lengths.as_slice() {
        [] => 0,
        [one] => {
            if one.is_empty() || !one.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseError::BadContentLength);
            }
            one.parse::<usize>().map_err(|_| ParseError::BadContentLength)?
        }
        many => {
            if many.windows(2).any(|w| w[0] != w[1]) {
                return Err(ParseError::BadContentLength);
            }
            many[0].parse::<usize>().map_err(|_| ParseError::BadContentLength)?
        }
    };
    if len > MAX_BODY {
        return Err(ParseError::BodyTooLarge);
    }
    if body_buf.len() < len {
        return Ok(None);
    }
    req.body = body_buf[..len].to_vec();
    Ok(Some((req, body_start + len)))
}

/// Decode a chunked body from the front of `buf`. Returns the body and the
/// bytes used (including the trailer section), or `None` if incomplete.
pub fn decode_chunked(buf: &[u8]) -> Result<Option<(Vec<u8>, usize)>, ParseError> {
    let mut pos = 0;
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find(&buf[pos..], b"\r\n") else {
            if buf.len() - pos > 1024 {
                return Err(ParseError::BadChunk);
            }
            return Ok(None);
        };
        let line = std::str::from_utf8(&buf[pos..pos + line_end]).map_err(|_| ParseError::BadChunk)?;
        let size_str = line.split(';').next().unwrap_or("").trim();
        if size_str.is_empty() || size_str.len() > 8 {
            return Err(ParseError::BadChunk);
        }
        let size = usize::from_str_radix(size_str, 16).map_err(|_| ParseError::BadChunk)?;
        pos += line_end + 2;
        if size == 0 {
            // Trailer section: header lines until an empty line.
            loop {
                let Some(end) = find(&buf[pos..], b"\r\n") else { return Ok(None) };
                pos += end + 2;
                if end == 0 {
                    return Ok(Some((body, pos)));
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(ParseError::BodyTooLarge);
        }
        if buf.len() < pos + size + 2 {
            return Ok(None);
        }
        body.extend_from_slice(&buf[pos..pos + size]);
        if &buf[pos + size..pos + size + 2] != b"\r\n" {
            return Err(ParseError::BadChunk);
        }
        pos += size + 2;
    }
}

/// Encode `chunks` with chunked transfer coding (including the final chunk).
pub fn encode_chunked<'a>(chunks: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for c in chunks {
        if c.is_empty() {
            continue;
        }
        out.extend_from_slice(format!("{:x}\r\n", c.len()).as_bytes());
        out.extend_from_slice(c);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(raw: &[u8]) -> (Request, usize) {
        parse_request(raw).unwrap().expect("complete")
    }

    #[test]
    fn simple_get() {
        let raw = b"GET /index.html?x=1 HTTP/1.1\r\nHost: example.com\r\nAccept:  */*  \r\n\r\n";
        let (r, used) = parse_ok(raw);
        assert_eq!(used, raw.len());
        assert_eq!(r.method, "GET");
        assert_eq!(r.path(), "/index.html");
        assert_eq!(r.version, Version::Http11);
        assert_eq!(r.header("host"), Some("example.com"));
        assert_eq!(r.header("ACCEPT"), Some("*/*"));
        assert!(r.keep_alive());
        assert!(r.body.is_empty());
    }

    #[test]
    fn incomplete_head_and_body() {
        assert_eq!(parse_request(b"GET / HTTP/1.1\r\nHost: a\r\n").unwrap(), None);
        assert_eq!(parse_request(b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab").unwrap(), None);
    }

    #[test]
    fn every_prefix_is_incomplete_not_an_error() {
        let raw = b"POST /echo HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nX-Trailer: y\r\n\r\n";
        for i in 0..raw.len() {
            assert_eq!(parse_request(&raw[..i]), Ok(None), "prefix {i}");
        }
        let (r, used) = parse_ok(raw);
        assert_eq!(r.body, b"abcde");
        assert_eq!(used, raw.len());
    }

    #[test]
    fn content_length_body_and_pipelining() {
        let raw = b"POST /a HTTP/1.1\r\nContent-Length: 3\r\n\r\nxyzGET /b HTTP/1.1\r\n\r\n";
        let (r1, used) = parse_ok(raw);
        assert_eq!(r1.body, b"xyz");
        let (r2, used2) = parse_ok(&raw[used..]);
        assert_eq!(r2.target, "/b");
        assert_eq!(used + used2, raw.len());
    }

    #[test]
    fn keep_alive_rules() {
        let (r, _) = parse_ok(b"GET / HTTP/1.0\r\n\r\n");
        assert!(!r.keep_alive());
        let (r, _) = parse_ok(b"GET / HTTP/1.0\r\nConnection: Keep-Alive\r\n\r\n");
        assert!(r.keep_alive());
        let (r, _) = parse_ok(b"GET / HTTP/1.1\r\nConnection: upgrade, close\r\n\r\n");
        assert!(!r.keep_alive());
    }

    #[test]
    fn rejects_malformed() {
        let cases: &[(&[u8], ParseError)] = &[
            (b"GET /\r\n\r\n", ParseError::BadRequestLine),
            (b"GET  / HTTP/1.1\r\n\r\n", ParseError::BadRequestLine),
            (b"G(T / HTTP/1.1\r\n\r\n", ParseError::BadRequestLine),
            (b"GET / HTTP/2.0\r\n\r\n", ParseError::BadVersion),
            (b"GET / HTTP/1.1\r\nNoColon\r\n\r\n", ParseError::BadHeader),
            (b"GET / HTTP/1.1\r\nBad Name: x\r\n\r\n", ParseError::BadHeader),
            (b"GET / HTTP/1.1\r\n folded: x\r\n\r\n", ParseError::BadHeader),
            (b"POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n", ParseError::BadContentLength),
            (b"POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n", ParseError::BadContentLength),
            (
                b"POST / HTTP/1.1\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
                ParseError::BadContentLength,
            ),
            (b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n", ParseError::UnsupportedTransferEncoding),
            (b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n", ParseError::BadChunk),
            (b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabc\r\n", ParseError::BadChunk),
        ];
        for (raw, want) in cases {
            assert_eq!(parse_request(raw), Err(want.clone()), "{}", String::from_utf8_lossy(raw));
        }
    }

    #[test]
    fn limits() {
        let big = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD));
        assert_eq!(parse_request(big.as_bytes()), Err(ParseError::HeadTooLarge));
        let huge = format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert_eq!(parse_request(huge.as_bytes()), Err(ParseError::BodyTooLarge));
        assert_eq!(ParseError::BodyTooLarge.status(), 413);
    }

    #[test]
    fn leading_crlf_is_skipped() {
        let (r, used) = parse_ok(b"\r\n\r\nGET / HTTP/1.1\r\n\r\n");
        assert_eq!(r.method, "GET");
        assert_eq!(used, 22);
    }

    #[test]
    fn chunked_roundtrip() {
        let chunks: [&[u8]; 3] = [b"hello ", b"", b"chunked world"];
        let enc = encode_chunked(chunks);
        assert_eq!(enc, b"6\r\nhello \r\nd\r\nchunked world\r\n0\r\n\r\n");
        let (body, used) = decode_chunked(&enc).unwrap().unwrap();
        assert_eq!(body, b"hello chunked world");
        assert_eq!(used, enc.len());
    }
}
