//! TCP bridge server (port 5333) — frame-level forwarding for Device Console compatibility.

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

            loop {
                // Try to decode frames from buffered data first
                while let Some((payload, consumed)) = packet::decode_tcp_frame(&read_buf) {
                    read_buf.drain(..consumed);

                    let device = state.get_device(0).await;
                    if let Some(client) = device {
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

                // Read more data from the socket
                let n = match stream.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!("TCP bridge read error: {e}");
                        break;
                    }
                };
                read_buf.extend_from_slice(&tmp[..n]);
            }
            tracing::info!("TCP bridge connection closed from {peer}");
        });
    }
}
