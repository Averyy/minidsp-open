//! Transport trait and implementations for USB HID and TCP.

use std::future::Future;
use std::pin::Pin;

/// Error type for transport operations.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("snapshot context precondition failed")]
    PreconditionFailed,
    #[error("I/O error: {0}")]
    Io(String),
    #[error("USB HID error: {0}")]
    Hid(String),
    #[error("device disconnected")]
    Disconnected,
    #[error("timeout")]
    Timeout,
    #[error("device not found")]
    NotFound,
    #[error("device busy (queue full)")]
    Busy,
}

pub type ReconnectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Transport>, TransportError>> + Send + 'a>>;

/// Async transport trait — abstracts USB HID and TCP connections.
///
/// The `send_frame` and `recv_frame` methods operate on raw command payloads
/// (not wrapped in HID reports or TCP framing). Each transport implementation
/// handles its own wire encoding.
pub trait Transport: Send + Sync + 'static {
    /// Send a command payload to the device. The transport handles wire encoding
    /// (HID wrapping for USB, frame encoding for TCP).
    fn send_frame(
        &self,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>;

    /// Receive a response payload from the device. The transport handles wire
    /// decoding (HID unwrapping for USB, frame decoding for TCP).
    /// Returns the decoded payload bytes.
    fn recv_frame(
        &self,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>>;

    /// Check if the transport is still connected.
    fn is_connected(&self) -> bool;

    /// Close the transport.
    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>>;

    /// Reopen this connection after an ambiguous exchange or disconnect.
    fn reconnect(&self) -> ReconnectFuture<'_> {
        Box::pin(async move {
            let _ = self.close().await;
            crate::queue::open_transport(&self.url()).await
        })
    }

    /// Transport description (for probe output).
    fn url(&self) -> String;
}

pub mod hid;
pub mod tcp;
pub mod ws;
