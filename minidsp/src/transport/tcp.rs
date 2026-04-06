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

    /// Connect via Unix socket path. The Unix socket serves the same
    /// TCP bridge protocol as port 5333 — not HTTP.
    /// This is for raw frame transport over a Unix domain socket.
    #[cfg(unix)]
    pub async fn connect_unix(path: &str) -> Result<Self, TransportError> {
        use tokio::net::UnixStream;
        let unix_stream = UnixStream::connect(path)
            .await
            .map_err(|e| TransportError::Io(format!("unix socket {path}: {e}")))?;
        // Split unix stream and wrap in our transport
        // We need TcpStream but have UnixStream — both implement AsyncRead/Write.
        // For now, convert via the raw fd trick on Unix
        let _std_stream = unix_stream.into_std()
            .map_err(|e| TransportError::Io(e.to_string()))?;
        // Re-wrap as tokio UnixStream — we'll store it separately
        // Actually the simplest approach: since our transport uses TcpStream internally,
        // and both TcpStream and UnixStream implement the same traits,
        // we should refactor to use a generic async stream. For now, fall through.
        Err(TransportError::Io(format!("Unix socket at {path} — not yet implemented, falling through to USB")))
    }
}

impl Transport for TcpTransport {
    fn send_frame(&self, payload: &[u8]) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let stream = self.stream.clone();
        let frame = packet::encode_frame(payload);
        Box::pin(async move {
            let mut stream = stream.lock().await;
            stream
                .write_all(&frame)
                .await
                .map_err(|e| TransportError::Io(e.to_string()))?;
            stream
                .flush()
                .await
                .map_err(|e| TransportError::Io(e.to_string()))?;
            Ok(())
        })
    }

    fn recv_frame(&self, timeout_ms: u64) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
        let stream = self.stream.clone();
        let read_buf = self.read_buf.clone();
        Box::pin(async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

            loop {
                // Try to decode a frame from buffered data
                {
                    let mut buf = read_buf.lock().await;
                    if let Some((payload, consumed)) = packet::decode_tcp_frame(&buf) {
                        buf.drain(..consumed);
                        return Ok(payload);
                    }
                }

                // Need more data — read with timeout
                let mut tmp = [0u8; 256];
                let mut stream = stream.lock().await;
                let result = tokio::time::timeout_at(
                    deadline,
                    stream.read(&mut tmp),
                )
                .await;

                match result {
                    Ok(Ok(0)) => return Err(TransportError::Disconnected),
                    Ok(Ok(n)) => {
                        drop(stream);
                        let mut buf = read_buf.lock().await;
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    Ok(Err(e)) => return Err(TransportError::Io(e.to_string())),
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
            stream
                .shutdown()
                .await
                .map_err(|e| TransportError::Io(e.to_string()))
        })
    }

    fn url(&self) -> String {
        format!("tcp://{}", self.addr)
    }
}
