//! Async server on Tokio. Same parser, same handler, same state machine.

use crate::app::App;
use crate::{next_response, Step, IDLE_TIMEOUT};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub async fn serve(listener: TcpListener, app: Arc<App>) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            let _ = handle_connection(stream, &app).await;
        });
    }
}

pub async fn handle_connection(mut stream: TcpStream, app: &App) -> io::Result<()> {
    stream.set_nodelay(true)?;
    let mut buf = Vec::new();
    let mut tmp = vec![0u8; 8192];
    loop {
        loop {
            // Static files are read with blocking std::fs inside the handler;
            // acceptable for a demo, see README.
            match next_response(&mut buf, app) {
                Step::NeedMore => break,
                Step::Respond { bytes, close } => {
                    stream.write_all(&bytes).await?;
                    if close {
                        return Ok(());
                    }
                }
            }
        }
        let n = match tokio::time::timeout(IDLE_TIMEOUT, stream.read(&mut tmp)).await {
            Err(_) => return Ok(()), // idle timeout
            Ok(Ok(0)) => return Ok(()),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
        };
        buf.extend_from_slice(&tmp[..n]);
    }
}
