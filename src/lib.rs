//! HTTP/1.1 from scratch: an I/O-free parser ([`parser`]), responses
//! ([`response`]), a request handler with static files ([`app`]), and two
//! servers that share all of it: [`server_std`] (blocking sockets + thread
//! pool) and [`server_tokio`] (async).

pub mod app;
pub mod parser;
pub mod response;
pub mod server_std;
pub mod server_tokio;
pub mod threadpool;

use app::App;
use parser::parse_request;
use response::Response;
use std::time::Duration;

/// How long an idle keep-alive connection is kept open.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// One step of the per-connection state machine.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// The buffer does not hold a full request yet: read more bytes.
    NeedMore,
    /// Write these bytes; close the connection afterwards if `close`.
    Respond { bytes: Vec<u8>, close: bool },
}

/// Try to take one request off the front of `buf` and produce its response.
/// Both servers call this in a loop, which gives pipelining for free.
pub fn next_response(buf: &mut Vec<u8>, app: &App) -> Step {
    match parse_request(buf) {
        Ok(None) => Step::NeedMore,
        Ok(Some((req, used))) => {
            buf.drain(..used);
            let resp = app.handle(&req);
            let keep = req.keep_alive();
            Step::Respond { bytes: resp.to_bytes(keep, req.method == "HEAD"), close: !keep }
        }
        Err(e) => {
            buf.clear();
            Step::Respond { bytes: Response::error(e.status()).to_bytes(false, false), close: true }
        }
    }
}
