//! Blocking server: `std::net::TcpListener` + a fixed thread pool.

use crate::app::App;
use crate::threadpool::ThreadPool;
use crate::{next_response, Step, IDLE_TIMEOUT};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// Accept connections forever, handing each one to the pool.
pub fn serve(listener: TcpListener, app: Arc<App>, workers: usize) -> io::Result<()> {
    let pool = ThreadPool::new(workers);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let app = app.clone();
        pool.execute(move || {
            let _ = handle_connection(stream, &app);
        });
    }
    Ok(())
}

pub fn handle_connection(mut stream: TcpStream, app: &App) -> io::Result<()> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    stream.set_nodelay(true)?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        loop {
            match next_response(&mut buf, app) {
                Step::NeedMore => break,
                Step::Respond { bytes, close } => {
                    stream.write_all(&bytes)?;
                    if close {
                        return Ok(());
                    }
                }
            }
        }
        let n = match stream.read(&mut tmp) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(()),
            Err(e) => return Err(e),
        };
        buf.extend_from_slice(&tmp[..n]);
    }
}
