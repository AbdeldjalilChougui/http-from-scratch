# http-from-scratch

An HTTP/1.1 server written from the socket up, twice: once on `std::net::TcpListener`
with a hand-made thread pool, and once on Tokio. Both share the **same I/O-free parser,
handler and connection state machine**, so the only difference is how bytes move.

## Quick start

```sh
cargo run --release -- --root public --port 8080            # std + thread pool
cargo run --release -- --tokio --root public --port 8080    # Tokio
curl -v http://127.0.0.1:8080/
curl -d 'hello' http://127.0.0.1:8080/echo
curl -H 'Transfer-Encoding: chunked' -d @README.md http://127.0.0.1:8080/echo
curl --raw http://127.0.0.1:8080/chunked
```

Routes: `GET/HEAD /<path>` serves files under `--root` (`index.html` for directories),
`POST /echo` echoes the body, `GET /chunked` streams a chunked response.

## How it works

- `parser.rs`: incremental parser. Feed it the bytes received so far; it returns
  `Ok(None)` until a full request (head + body) is there, then the request and the number
  of bytes consumed, so pipelined requests stay in the buffer. Handles `Content-Length`
  and `Transfer-Encoding: chunked` (with extensions and trailers), rejects malformed request
  lines/headers, obsolete line folding, conflicting `Content-Length`s, and requests carrying
  both CL and TE (request-smuggling guard). Size limits answer 431 / 413.
- `response.rs`: status line, headers, `Content-Length` or chunked bodies, HEAD handling.
- `app.rs`: routing and static files with percent-decoding and path traversal protection
  (`..`, encoded `%2e%2e`, backslashes and NUL bytes are refused), MIME types by extension.
- `lib.rs::next_response`: the shared per-connection step (parse → handle → serialize,
  keep-alive decision). Both servers call it in a loop.
- `threadpool.rs`: fixed pool over an `mpsc` channel with graceful shutdown on drop;
  a panicking job doesn't kill its worker.
- `server_std.rs` / `server_tokio.rs`: read loops with a 5 s idle keep-alive timeout.

## Tests

`cargo test` runs parser/response/path unit tests (including "every prefix of a valid
request is incomplete, never an error") and `tests/servers.rs`, which starts **both**
servers on ephemeral ports and checks static files, 301/403/404/405/400/505, HEAD,
keep-alive, pipelining, HTTP/1.0, chunked requests sent in pieces, chunked responses and
16 concurrent keep-alive clients.

## Status / limitations

Learning project, not a production server.
- No TLS, no HTTP/2, no `Expect: 100-continue`, no compression, no range requests,
  no caching headers (`ETag`/`Last-Modified`).
- Whole bodies and files are buffered in memory (8 MiB request body limit).
- In the std server a keep-alive connection occupies a pool thread until it closes or idles
  out (5 s), so slow clients can exhaust the pool.
- The Tokio server reads files with blocking `std::fs` inside the async task.

## License

MIT, see [LICENSE](LICENSE).
