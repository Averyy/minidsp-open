//! TCP bridge server (port 5333) — frame-level forwarding for Device Console compatibility.
//!
//! WI-DG compatibility: if the first received chunk is >= 64 bytes, enables
//! 64-byte chunking mode. Non-zero padding between frames is skipped.

use crate::state::DaemonState;
use minidsp::queue::Priority;
use minidsp_protocol::packet;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Run the TCP bridge server on the given address.
pub async fn serve(addr: &str, state: Arc<DaemonState>) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("TCP bridge listening on {addr}");

    // Single-connection model (WI-DG compat)
    loop {
        let (mut stream, peer) = listener.accept().await?;
        tracing::info!("TCP bridge connection from {peer}");

        let state = state.clone();
        tokio::spawn(async move {
            let mut read_buf = Vec::with_capacity(512);
            let mut tmp = [0u8; 512];
            let mut chunked_mode = false;
            let mut first_read = true;

            loop {
                // Try to decode frames from buffered data first
                loop {
                    if chunked_mode {
                        // WI-DG 64-byte chunking mode: process in 64-byte blocks
                        if read_buf.len() < 64 {
                            break;
                        }
                        let chunk = read_buf[..64].to_vec();
                        read_buf.drain(..64);

                        // Skip non-zero padding bytes that don't start a valid frame
                        // Find the first byte that could be a frame length
                        let frame_start = chunk.iter().position(|&b| b > 0 && b < 64);
                        let payload = if let Some(start) = frame_start {
                            packet::decode_frame(&chunk[start..])
                        } else {
                            None
                        };

                        if let Some(payload) = payload {
                            if let Some(client) = state.get_device(0).await {
                                match client.queue().execute_raw(payload, Priority::Normal).await {
                                    Ok(raw_resp) => {
                                        // Respond in 64-byte padded frame for WI-DG
                                        let frame = packet::encode_frame(&raw_resp);
                                        let mut padded = vec![0u8; 64];
                                        let copy_len = frame.len().min(64);
                                        padded[..copy_len].copy_from_slice(&frame[..copy_len]);
                                        if let Err(e) = stream.write_all(&padded).await {
                                            tracing::warn!("TCP bridge write error: {e}");
                                            return;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("TCP bridge command error: {e}");
                                    }
                                }
                            }
                        }
                    } else {
                        // Standard framing: skip non-zero padding between frames
                        let decoded = decode_tcp_frame_skip_padding(&read_buf);
                        match decoded {
                            Some((payload, consumed)) => {
                                read_buf.drain(..consumed);

                                if let Some(client) = state.get_device(0).await {
                                    match client.queue().execute_raw(payload, Priority::Normal).await {
                                        Ok(raw_resp) => {
                                            let frame = packet::encode_tcp_response(&raw_resp);
                                            if let Err(e) = stream.write_all(&frame).await {
                                                tracing::warn!("TCP bridge write error: {e}");
                                                return;
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("TCP bridge command error: {e}");
                                        }
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                }

                // Read more data from the socket
                let n = match stream.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("TCP bridge read error: {e}");
                        break;
                    }
                };

                // WI-DG detection: if first chunk is >= 64 bytes, enable chunking mode
                if first_read {
                    first_read = false;
                    if n >= 64 {
                        chunked_mode = true;
                        tracing::info!("TCP bridge: WI-DG 64-byte chunking mode detected");
                    }
                }

                read_buf.extend_from_slice(&tmp[..n]);
            }
            tracing::info!("TCP bridge connection closed from {peer}");
        });
    }
}

/// Decode a TCP frame, skipping any non-zero padding bytes that don't form valid frames.
/// This handles WI-DG devices that may insert non-zero bytes between frames.
fn decode_tcp_frame_skip_padding(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    if data.is_empty() {
        return None;
    }

    // First try standard decoding (which already skips zero padding)
    if let Some(result) = packet::decode_tcp_frame(data) {
        return Some(result);
    }

    // Try skipping non-zero bytes that don't start a valid frame
    for offset in 1..data.len() {
        if let Some((payload, consumed)) = packet::decode_tcp_frame(&data[offset..]) {
            return Some((payload, offset + consumed));
        }
    }

    None
}
