//! HTTP response building and serialization.

use crate::parser::encode_chunked;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// Sent with `Content-Length`.
    Full(Vec<u8>),
    /// Sent with `Transfer-Encoding: chunked`, one chunk per element.
    Chunked(Vec<Vec<u8>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            headers: vec![("Content-Type".into(), content_type.into())],
            body: Body::Full(body.into()),
        }
    }

    pub fn text(status: u16, body: &str) -> Response {
        Response::new(status, "text/plain; charset=utf-8", body)
    }

    pub fn error(status: u16) -> Response {
        Response::text(status, &format!("{status} {}\n", reason(status)))
    }

    pub fn header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Serialize the status line, headers and (unless `head_only`) the body.
    /// `keep_alive` decides the `Connection` header.
    pub fn to_bytes(&self, keep_alive: bool, head_only: bool) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status)).into_bytes();
        for (k, v) in &self.headers {
            out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        match &self.body {
            Body::Full(b) => out.extend_from_slice(format!("Content-Length: {}\r\n", b.len()).as_bytes()),
            Body::Chunked(_) => out.extend_from_slice(b"Transfer-Encoding: chunked\r\n"),
        }
        out.extend_from_slice(if keep_alive { b"Connection: keep-alive\r\n" } else { b"Connection: close\r\n" });
        out.extend_from_slice(b"Server: http-from-scratch\r\n\r\n");
        if !head_only {
            match &self.body {
                Body::Full(b) => out.extend_from_slice(b),
                Body::Chunked(chunks) => out.extend_from_slice(&encode_chunked(chunks.iter().map(Vec::as_slice))),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_full_body() {
        let r = Response::text(200, "hi");
        let s = String::from_utf8(r.to_bytes(true, false)).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Content-Length: 2\r\n"));
        assert!(s.contains("Connection: keep-alive\r\n"));
        assert!(s.ends_with("\r\n\r\nhi"));
    }

    #[test]
    fn head_only_omits_body_but_keeps_length() {
        let s = String::from_utf8(Response::text(200, "hello").to_bytes(false, true)).unwrap();
        assert!(s.contains("Content-Length: 5\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn chunked_body() {
        let r = Response { status: 200, headers: vec![], body: Body::Chunked(vec![b"ab".to_vec(), b"c".to_vec()]) };
        let s = String::from_utf8(r.to_bytes(true, false)).unwrap();
        assert!(s.contains("Transfer-Encoding: chunked\r\n"));
        assert!(s.ends_with("\r\n\r\n2\r\nab\r\n1\r\nc\r\n0\r\n\r\n"));
    }
}
