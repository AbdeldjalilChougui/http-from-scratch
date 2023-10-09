//! The request handler shared by both servers: a few demo routes plus a
//! static file server rooted at a directory.

use crate::parser::Request;
use crate::response::{Body, Response};
use std::path::{Component, Path, PathBuf};

pub struct App {
    pub root: PathBuf,
}

/// Percent-decode a URL path. Returns `None` on malformed escapes or non-UTF-8.
pub fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Map a request path to a relative filesystem path, rejecting anything that
/// could escape the root (`..`, absolute components, NUL bytes, backslashes).
pub fn safe_relative_path(url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let mut out = PathBuf::new();
    for comp in Path::new(decoded.trim_start_matches('/')).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

pub fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

impl App {
    pub fn new(root: impl Into<PathBuf>) -> App {
        App { root: root.into() }
    }

    pub fn handle(&self, req: &Request) -> Response {
        match (req.method.as_str(), req.path()) {
            ("POST", "/echo") => {
                let ct = req.header("content-type").unwrap_or("application/octet-stream");
                Response::new(200, ct, req.body.clone())
            }
            ("GET", "/chunked") => Response {
                status: 200,
                headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())],
                body: Body::Chunked((1..=5).map(|i| format!("chunk {i}\n").into_bytes()).collect()),
            },
            ("GET" | "HEAD", path) => self.static_file(path),
            _ => Response::error(405).header("Allow", "GET, HEAD, POST"),
        }
    }

    fn static_file(&self, url_path: &str) -> Response {
        let Some(rel) = safe_relative_path(url_path) else {
            return Response::error(403);
        };
        let mut full = self.root.join(&rel);
        if full.is_dir() {
            if !url_path.ends_with('/') {
                return Response::error(301).header("Location", &format!("{url_path}/"));
            }
            full.push("index.html");
        }
        match std::fs::read(&full) {
            Ok(bytes) => Response::new(200, content_type(&full), bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Response::error(404),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Response::error(403),
            Err(_) => Response::error(500),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_sanitizing() {
        assert_eq!(safe_relative_path("/a/b.txt"), Some(PathBuf::from("a/b.txt")));
        assert_eq!(safe_relative_path("/./a/%20b"), Some(PathBuf::from("a/ b")));
        assert_eq!(safe_relative_path("/"), Some(PathBuf::new()));
        assert_eq!(safe_relative_path("/../etc/passwd"), None);
        assert_eq!(safe_relative_path("/a/%2e%2e/%2e%2e/x"), None);
        assert_eq!(safe_relative_path("/a%00b"), None);
        assert_eq!(safe_relative_path("/a%5c..%5cb"), None);
        assert_eq!(safe_relative_path("/%zz"), None);
    }

    #[test]
    fn mime_types() {
        assert_eq!(content_type(Path::new("x.HTML")), "text/html; charset=utf-8");
        assert_eq!(content_type(Path::new("x.bin")), "application/octet-stream");
    }
}
