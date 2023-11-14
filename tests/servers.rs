//! End-to-end tests over real sockets, run against both servers.

use http_from_scratch::app::App;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

fn site_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hfs-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("hello.txt"), "hello file\n").unwrap();
    std::fs::write(dir.join("sub/index.html"), "<p>sub index</p>").unwrap();
    dir
}

fn std_server() -> SocketAddr {
    static ADDR: OnceLock<SocketAddr> = OnceLock::new();
    *ADDR.get_or_init(|| {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let app = Arc::new(App::new(site_root()));
        std::thread::spawn(move || http_from_scratch::server_std::serve(l, app, 4));
        addr
    })
}

fn tokio_server() -> SocketAddr {
    static ADDR: OnceLock<SocketAddr> = OnceLock::new();
    *ADDR.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(l.local_addr().unwrap()).unwrap();
                let app = Arc::new(App::new(site_root()));
                let _ = http_from_scratch::server_tokio::serve(l, app).await;
            });
        });
        rx.recv().unwrap()
    })
}

fn servers() -> [(&'static str, SocketAddr); 2] {
    [("std", std_server()), ("tokio", tokio_server())]
}

fn connect(addr: SocketAddr) -> TcpStream {
    let s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

/// Minimal client-side response reader: status, headers, body (by
/// Content-Length or chunked). Reads exactly one response from `s`.
struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_response(s: &mut TcpStream, buf: &mut Vec<u8>, head_request: bool) -> Resp {
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = s.read(&mut tmp).unwrap();
        assert!(n > 0, "connection closed before head");
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8(buf[..head_end].to_vec()).unwrap();
    buf.drain(..head_end + 4);
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers: Vec<(String, String)> = lines
        .map(|l| {
            let (k, v) = l.split_once(':').unwrap();
            (k.to_string(), v.trim().to_string())
        })
        .collect();
    let mut r = Resp { status, headers, body: Vec::new() };
    if head_request {
        return r;
    }
    if r.header("transfer-encoding") == Some("chunked") {
        loop {
            if let Some((body, used)) = http_from_scratch::parser::decode_chunked(buf).unwrap() {
                r.body = body;
                buf.drain(..used);
                return r;
            }
            let n = s.read(&mut tmp).unwrap();
            assert!(n > 0);
            buf.extend_from_slice(&tmp[..n]);
        }
    }
    let len: usize = r.header("content-length").unwrap().parse().unwrap();
    while buf.len() < len {
        let n = s.read(&mut tmp).unwrap();
        assert!(n > 0);
        buf.extend_from_slice(&tmp[..n]);
    }
    r.body = buf.drain(..len).collect();
    r
}

fn roundtrip(addr: SocketAddr, raw: &str) -> Resp {
    let mut s = connect(addr);
    s.write_all(raw.as_bytes()).unwrap();
    read_response(&mut s, &mut Vec::new(), raw.starts_with("HEAD"))
}

#[test]
fn serves_static_files() {
    for (name, addr) in servers() {
        let r = roundtrip(addr, "GET /hello.txt HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(r.status, 200, "{name}");
        assert_eq!(r.body, b"hello file\n", "{name}");
        assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));

        let r = roundtrip(addr, "GET /sub/ HTTP/1.1\r\n\r\n");
        assert_eq!(r.body, b"<p>sub index</p>", "{name}");
        let r = roundtrip(addr, "GET /sub HTTP/1.1\r\n\r\n");
        assert_eq!((r.status, r.header("location")), (301, Some("/sub/")), "{name}");
        assert_eq!(roundtrip(addr, "GET /missing HTTP/1.1\r\n\r\n").status, 404, "{name}");
        assert_eq!(roundtrip(addr, "GET /../Cargo.toml HTTP/1.1\r\n\r\n").status, 403, "{name}");
        assert_eq!(roundtrip(addr, "GET /%2e%2e/x HTTP/1.1\r\n\r\n").status, 403, "{name}");
    }
}

#[test]
fn head_has_length_but_no_body() {
    for (name, addr) in servers() {
        let mut s = connect(addr);
        s.write_all(b"HEAD /hello.txt HTTP/1.1\r\n\r\nGET /hello.txt HTTP/1.1\r\n\r\n").unwrap();
        let mut buf = Vec::new();
        let r = read_response(&mut s, &mut buf, true);
        assert_eq!(r.header("content-length"), Some("11"), "{name}");
        // The next bytes must be the second response, proving HEAD sent no body.
        let r2 = read_response(&mut s, &mut buf, false);
        assert_eq!(r2.body, b"hello file\n", "{name}");
    }
}

#[test]
fn keep_alive_and_pipelining() {
    for (name, addr) in servers() {
        let mut s = connect(addr);
        let mut buf = Vec::new();
        // Two sequential requests on one connection.
        for _ in 0..2 {
            s.write_all(b"GET /hello.txt HTTP/1.1\r\n\r\n").unwrap();
            let r = read_response(&mut s, &mut buf, false);
            assert_eq!(r.status, 200, "{name}");
            assert_eq!(r.header("connection"), Some("keep-alive"));
        }
        // Three pipelined requests in a single write; the last one closes.
        s.write_all(
            b"GET /hello.txt HTTP/1.1\r\n\r\nPOST /echo HTTP/1.1\r\nContent-Length: 4\r\n\r\npingGET /hello.txt HTTP/1.1\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        assert_eq!(read_response(&mut s, &mut buf, false).status, 200);
        assert_eq!(read_response(&mut s, &mut buf, false).body, b"ping", "{name}");
        let last = read_response(&mut s, &mut buf, false);
        assert_eq!(last.header("connection"), Some("close"));
        let mut rest = Vec::new();
        s.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty() && buf.is_empty(), "{name}: server should close");
    }
}

#[test]
fn http10_closes_by_default() {
    for (name, addr) in servers() {
        let mut s = connect(addr);
        s.write_all(b"GET /hello.txt HTTP/1.0\r\n\r\n").unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).unwrap();
        let text = String::from_utf8_lossy(&all);
        assert!(text.contains("Connection: close") && text.ends_with("hello file\n"), "{name}: {text}");
    }
}

#[test]
fn chunked_request_and_response() {
    for (name, addr) in servers() {
        let mut s = connect(addr);
        // Send the chunked body in pieces to exercise incremental parsing.
        let parts: [&[u8]; 4] = [
            b"POST /echo HTTP/1.1\r\nTransfer-Encoding: chunked\r\nContent-Type: text/plain\r\n\r\n5\r\nhel",
            b"lo\r\n6\r\n world\r\n",
            b"0\r\n",
            b"\r\n",
        ];
        for p in parts {
            s.write_all(p).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut buf = Vec::new();
        let r = read_response(&mut s, &mut buf, false);
        assert_eq!(r.body, b"hello world", "{name}");
        assert_eq!(r.header("content-type"), Some("text/plain"));

        s.write_all(b"GET /chunked HTTP/1.1\r\n\r\n").unwrap();
        let r = read_response(&mut s, &mut buf, false);
        assert_eq!(r.header("transfer-encoding"), Some("chunked"));
        assert_eq!(String::from_utf8(r.body).unwrap(), "chunk 1\nchunk 2\nchunk 3\nchunk 4\nchunk 5\n", "{name}");
    }
}

#[test]
fn errors() {
    for (name, addr) in servers() {
        assert_eq!(roundtrip(addr, "BREW /pot HTTP/1.1\r\n\r\n").status, 405, "{name}");
        assert_eq!(roundtrip(addr, "GARBAGE\r\n\r\n").status, 400, "{name}");
        assert_eq!(roundtrip(addr, "GET / HTTP/3.0\r\n\r\n").status, 505, "{name}");
    }
}

#[test]
fn many_concurrent_clients() {
    for (name, addr) in servers() {
        let handles: Vec<_> = (0..16)
            .map(|_| {
                std::thread::spawn(move || {
                    let mut s = connect(addr);
                    let mut buf = Vec::new();
                    for _ in 0..5 {
                        s.write_all(b"GET /hello.txt HTTP/1.1\r\n\r\n").unwrap();
                        assert_eq!(read_response(&mut s, &mut buf, false).status, 200);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap_or_else(|_| panic!("{name} client failed"));
        }
    }
}
