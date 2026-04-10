//! WebSocket transport — connects to daemon's /devices/{id}/ws endpoint.
//!
//! Used for --daemon-url and --daemon-sock connections. The daemon serves
//! HTTP+WS on port 5380 and Unix socket. This transport upgrades to WebSocket
//! and sends/receives binary MiniDSP protocol frames.

use super::{Transport, TransportError};
use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use minidsp_protocol::packet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// WebSocket transport that connects to the daemon's raw frame bridge.
/// Uses type-erased async IO so it works over both TCP and Unix sockets.
pub struct WsTransport {
    sink: Arc<Mutex<SplitSink<WebSocketStream<Box<dyn AsyncRw>>, Message>>>,
    stream: Arc<Mutex<SplitStream<WebSocketStream<Box<dyn AsyncRw>>>>>,
    url: String,
}

/// Helper trait alias for async read+write+send+unpin.
trait AsyncRw: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncRw for T {}

fn map_ws_error(err: WsError) -> TransportError {
    match err {
        WsError::ConnectionClosed | WsError::AlreadyClosed => TransportError::Disconnected,
        WsError::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::UnexpectedEof => TransportError::Disconnected,
            _ => TransportError::Io(format!("WS I/O: {io_err}")),
        },
        other => TransportError::Io(format!("WS error: {other}")),
    }
}

impl WsTransport {
    /// Connect to a daemon WebSocket endpoint over TCP.
    /// `base_url` can be "http://host:5380", "ws://host:5380", or just "host:5380".
    /// `device_id` selects which device to connect to (default 0).
    pub async fn connect(base_url: &str, device_id: usize) -> Result<Self, TransportError> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        // Reject TLS schemes — we don't support wss:// yet
        if base_url.starts_with("https://") || base_url.starts_with("wss://") {
            return Err(TransportError::Io(
                "TLS (wss://, https://) not supported — use ws:// or http://".into(),
            ));
        }

        let ws_url = format_ws_url(base_url, device_id);

        let stripped = ws_url.trim_start_matches("ws://");
        let host_port = stripped.split('/').next().unwrap_or(stripped);
        let tcp_stream = tokio::net::TcpStream::connect(host_port)
            .await
            .map_err(|e| TransportError::Io(format!("TCP connect to {host_port}: {e}")))?;

        let request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| TransportError::Io(format!("WS request: {e}")))?;

        let boxed: Box<dyn AsyncRw> = Box::new(tcp_stream);
        let (ws_stream, _) = tokio_tungstenite::client_async(request, boxed)
            .await
            .map_err(|e| TransportError::Io(format!("WebSocket upgrade: {e}")))?;

        let (sink, stream) = ws_stream.split();
        Ok(Self {
            sink: Arc::new(Mutex::new(sink)),
            stream: Arc::new(Mutex::new(stream)),
            url: ws_url,
        })
    }

    /// Connect via Unix socket to daemon's WebSocket endpoint.
    #[cfg(unix)]
    pub async fn connect_unix(path: &str, device_id: usize) -> Result<Self, TransportError> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let unix_stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| TransportError::Io(format!("unix socket {path}: {e}")))?;

        let ws_url = format!("ws://localhost/devices/{device_id}/ws");
        let request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| TransportError::Io(format!("WS request: {e}")))?;

        let boxed: Box<dyn AsyncRw> = Box::new(unix_stream);
        let (ws_stream, _) = tokio_tungstenite::client_async(request, boxed)
            .await
            .map_err(|e| TransportError::Io(format!("WebSocket upgrade over unix: {e}")))?;

        let (sink, stream) = ws_stream.split();
        Ok(Self {
            sink: Arc::new(Mutex::new(sink)),
            stream: Arc::new(Mutex::new(stream)),
            url: format!("ws+unix:{path}/devices/{device_id}/ws"),
        })
    }
}

/// Normalize a base URL to a WebSocket URL for the raw frame bridge.
fn format_ws_url(base: &str, device_id: usize) -> String {
    if (base.starts_with("ws://") || base.starts_with("http://"))
        && base.contains("/devices/")
        && base.ends_with("/ws")
    {
        return base.replacen("http://", "ws://", 1);
    }

    let stripped = base
        .trim_start_matches("http://")
        .trim_start_matches("ws://");
    let host_port = stripped.split('/').next().unwrap_or(stripped);
    let hp = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:5380")
    };
    format!("ws://{hp}/devices/{device_id}/ws")
}

impl Transport for WsTransport {
    fn send_frame(
        &self,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let sink = self.sink.clone();
        let frame = packet::encode_frame(payload);
        tracing::debug!("WS OUT payload: {:02x?}", payload);
        tracing::trace!("WS OUT frame: {:02x?}", frame);
        Box::pin(async move {
            let mut sink = sink.lock().await;
            sink.send(Message::Binary(frame.into()))
                .await
                .map_err(map_ws_error)?;
            Ok(())
        })
    }

    fn recv_frame(
        &self,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
        let stream = self.stream.clone();
        Box::pin(async move {
            let deadline =
                tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
            // Hold lock for the entire receive operation (no concurrent recv callers)
            let mut stream = stream.lock().await;

            loop {
                let result = tokio::time::timeout_at(deadline, stream.next()).await;

                match result {
                    Ok(Some(Ok(Message::Binary(data)))) => {
                        tracing::trace!("WS IN frame: {:02x?}", data);
                        // Daemon sends encode_frame (with checksum) over WS
                        if let Some(payload) = packet::decode_frame(&data) {
                            tracing::debug!("WS IN payload: {:02x?}", payload);
                            return Ok(payload);
                        }
                        // Decode failure — corrupted frame, skip and try next
                        tracing::warn!(
                            "WS: undecodable binary frame ({} bytes), skipping",
                            data.len()
                        );
                        continue;
                    }
                    Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                        return Err(TransportError::Disconnected);
                    }
                    Ok(Some(Ok(_))) => continue, // Skip text/ping/pong
                    Ok(Some(Err(e))) => {
                        return Err(map_ws_error(e));
                    }
                    Err(_) => return Err(TransportError::Timeout),
                }
            }
        })
    }

    fn is_connected(&self) -> bool {
        true
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let sink = self.sink.clone();
        Box::pin(async move {
            let mut sink = sink.lock().await;
            sink.send(Message::Close(None))
                .await
                .map_err(map_ws_error)?;
            Ok(())
        })
    }

    fn url(&self) -> String {
        self.url.clone()
    }
}
