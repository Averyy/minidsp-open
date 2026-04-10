//! TCP transport for connecting to remote MiniDSP devices (via TCP bridge or WI-DG).

use super::{Transport, TransportError};
use minidsp_protocol::packet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// TCP transport with proper frame reassembly.
pub struct TcpTransport {
    stream: Arc<Mutex<TcpStream>>,
    read_buf: Arc<Mutex<Vec<u8>>>,
    addr: String,
}

impl TcpTransport {
    /// Connect to a remote MiniDSP device via TCP.
    pub async fn connect(addr: &str) -> Result<Self, TransportError> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(Self {
            stream: Arc::new(Mutex::new(stream)),
            read_buf: Arc::new(Mutex::new(Vec::with_capacity(256))),
            addr: addr.to_string(),
        })
    }

    // connect_unix removed — use WsTransport::connect_unix instead.
    // The daemon Unix socket serves HTTP/WS, not raw TCP bridge frames.
}

fn map_io_error(err: std::io::Error) -> TransportError {
    match err.kind() {
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::UnexpectedEof => TransportError::Disconnected,
        _ => TransportError::Io(err.to_string()),
    }
}

impl Transport for TcpTransport {
    fn send_frame(
        &self,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let stream = self.stream.clone();
        let frame = packet::encode_frame(payload);
        tracing::debug!("TCP OUT payload: {:02x?}", payload);
        tracing::trace!("TCP OUT frame: {:02x?}", frame);
        Box::pin(async move {
            let mut stream = stream.lock().await;
            stream.write_all(&frame).await.map_err(map_io_error)?;
            stream.flush().await.map_err(map_io_error)?;
            Ok(())
        })
    }

    fn recv_frame(
        &self,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
        let stream = self.stream.clone();
        let read_buf = self.read_buf.clone();
        Box::pin(async move {
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

            loop {
                // Try to decode a server response frame (no checksum)
                {
                    let mut buf = read_buf.lock().await;
                    if let Some((payload, consumed)) = packet::decode_tcp_response(&buf) {
                        tracing::trace!("TCP IN frame: {:02x?}", &buf[..consumed]);
                        tracing::debug!("TCP IN payload: {:02x?}", payload);
                        buf.drain(..consumed);
                        return Ok(payload);
                    }
                }

                // Need more data — read with timeout
                let mut tmp = [0u8; 256];
                let mut stream = stream.lock().await;
                let result = tokio::time::timeout_at(deadline, stream.read(&mut tmp)).await;

                match result {
                    Ok(Ok(0)) => return Err(TransportError::Disconnected),
                    Ok(Ok(n)) => {
                        drop(stream);
                        let mut buf = read_buf.lock().await;
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    Ok(Err(e)) => return Err(map_io_error(e)),
                    Err(_) => return Err(TransportError::Timeout),
                }
            }
        })
    }

    fn is_connected(&self) -> bool {
        true
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let stream = self.stream.clone();
        Box::pin(async move {
            let mut stream = stream.lock().await;
            stream.shutdown().await.map_err(map_io_error)
        })
    }

    fn url(&self) -> String {
        format!("tcp://{}", self.addr)
    }
}
