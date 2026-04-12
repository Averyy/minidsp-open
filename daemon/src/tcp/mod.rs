//! TCP bridge server (port 5333) — frame-level forwarding for Device Console
//! compatibility.
//!
//! Wi-DG behavior notes: if the first received chunk is >= 64 bytes, the
//! client is treated as a Wi-DG-style 64-byte chunked stream; otherwise the
//! standard length-prefixed framing applies. Non-zero padding between frames
//! is skipped so the real Wi-DG's wire-level quirks don't drop commands.
//!
//! Socket hygiene: `SO_REUSEADDR` is set before binding so a restarted
//! daemon doesn't spend 2× MSL stuck on a `TIME_WAIT` port. Each accepted
//! stream gets `TCP_NODELAY` (so 64-byte command writes go out immediately)
//! and `SO_KEEPALIVE` with a 120 s idle probe (so idle Device Console tabs
//! still register as disconnected if the client vanishes without FIN).
//!
//! Multi-client support: up to 4 simultaneous TCP clients are served. A
//! shared async mutex (`bridge_mutex`) is acquired around every raw HID
//! command so two clients cannot interleave USB traffic at the bridge layer.
//! The deeper `DaemonState` also serializes per-device I/O — this outer
//! mutex is belt-and-braces so we never trust that layering alone.

use crate::state::DaemonState;
use minidsp_protocol::packet;
use socket2::{Domain, SockRef, Socket, TcpKeepalive, Type};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Maximum simultaneous TCP clients before new accepts are refused.
///
/// Raised from 4 to 8 on 2026-04-11 after live testing showed the official
/// Device Console app briefly opens >4 parallel bridge sockets during its
/// initial probe burst (confirmed via daemon log: 8 connects in 20 ms). A
/// tighter cap caused spurious `refusing` warnings even though DC itself
/// recovered fine; 8 is comfortably above DC's observed peak and ezBEQ's
/// normal one-connection pattern, without letting a buggy client monopolise
/// the HID queue indefinitely.
const MAX_TCP_CLIENTS: usize = 8;

/// Keepalive idle before the OS sends the first probe.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(120);

/// Run the TCP bridge server on the given address.
pub async fn serve(addr: &str, state: Arc<DaemonState>) -> std::io::Result<()> {
    let listener = listen_reuseaddr(addr)?;
    tracing::info!("TCP bridge listening on {addr}");

    let bridge_mutex = Arc::new(Mutex::new(()));
    let active_clients = Arc::new(tokio::sync::Semaphore::new(MAX_TCP_CLIENTS));

    loop {
        let (stream, peer) = listener.accept().await?;

        // Apply per-connection sockopts before handing off to the worker.
        if let Err(e) = stream.set_nodelay(true) {
            tracing::warn!("TCP bridge: set_nodelay failed for {peer}: {e}");
        }
        if let Err(e) = enable_keepalive(&stream) {
            tracing::warn!("TCP bridge: keepalive setup failed for {peer}: {e}");
        }

        let permit = match active_clients.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(
                    "TCP bridge: refusing {peer} — already serving {MAX_TCP_CLIENTS} clients"
                );
                continue;
            }
        };

        tracing::info!("TCP bridge connection from {peer}");
        let state = state.clone();
        let bridge_mutex = bridge_mutex.clone();
        tokio::spawn(async move {
            let _permit = permit;
            handle_client(stream, peer, state, bridge_mutex).await;
        });
    }
}

async fn handle_client(
    mut stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
    state: Arc<DaemonState>,
    bridge_mutex: Arc<Mutex<()>>,
) {
    let mut read_buf = Vec::with_capacity(512);
    let mut tmp = [0u8; 512];
    let mut chunked_mode = false;
    let mut first_read = true;

    loop {
        // Try to decode frames from buffered data first.
        loop {
            if chunked_mode {
                if read_buf.len() < 64 {
                    break;
                }
                let chunk = read_buf[..64].to_vec();
                read_buf.drain(..64);

                let frame_start = chunk.iter().position(|&b| b > 0 && b < 64);
                let payload = frame_start.and_then(|start| packet::decode_frame(&chunk[start..]));

                if let Some(payload) = payload {
                    if let Some(client) = state.get_device(0).await {
                        let result = {
                            let _guard = bridge_mutex.lock().await;
                            crate::bridge::execute_raw_bridge_command(&state, &client, payload)
                                .await
                        };
                        match result {
                            Ok(raw_resp) => {
                                let frame = packet::encode_frame(&raw_resp);
                                let mut padded = vec![0u8; 64];
                                let copy_len = frame.len().min(64);
                                padded[..copy_len].copy_from_slice(&frame[..copy_len]);
                                if let Err(e) = stream.write_all(&padded).await {
                                    tracing::warn!("TCP bridge write error ({peer}): {e}");
                                    return;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("TCP bridge command error ({peer}): {e}");
                            }
                        }
                    }
                }
            } else {
                let decoded = decode_tcp_frame_skip_padding(&read_buf);
                match decoded {
                    Some((payload, consumed)) => {
                        read_buf.drain(..consumed);

                        if let Some(client) = state.get_device(0).await {
                            let result = {
                                let _guard = bridge_mutex.lock().await;
                                crate::bridge::execute_raw_bridge_command(&state, &client, payload)
                                    .await
                            };
                            match result {
                                Ok(raw_resp) => {
                                    let frame = packet::encode_tcp_response(&raw_resp);
                                    if let Err(e) = stream.write_all(&frame).await {
                                        tracing::warn!("TCP bridge write error ({peer}): {e}");
                                        return;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("TCP bridge command error ({peer}): {e}");
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
        }

        let n = match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("TCP bridge read error ({peer}): {e}");
                break;
            }
        };

        if first_read {
            first_read = false;
            if n >= 64 {
                chunked_mode = true;
                tracing::info!("TCP bridge ({peer}): Wi-DG 64-byte chunking mode detected");
            }
        }

        read_buf.extend_from_slice(&tmp[..n]);
    }
    tracing::info!("TCP bridge connection closed from {peer}");
}

/// Bind a TCP listener with `SO_REUSEADDR` so a daemon restart doesn't stall
/// on `TIME_WAIT` sockets. Parses the address as an IPv4 `host:port`.
fn listen_reuseaddr(addr: &str) -> std::io::Result<TcpListener> {
    let parsed: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let domain = match parsed {
        std::net::SocketAddr::V4(_) => Domain::IPV4,
        std::net::SocketAddr::V6(_) => Domain::IPV6,
    };
    let socket = Socket::new(domain, Type::STREAM, None)?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&parsed.into())?;
    socket.listen(128)?;

    let std_listener: std::net::TcpListener = socket.into();
    TcpListener::from_std(std_listener)
}

fn enable_keepalive(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    let sock = SockRef::from(stream);
    let keepalive = TcpKeepalive::new().with_time(KEEPALIVE_IDLE);
    sock.set_tcp_keepalive(&keepalive)
}

/// Decode a TCP frame, skipping any non-zero padding bytes that don't form valid frames.
/// This handles WI-DG devices that may insert non-zero bytes between frames.
fn decode_tcp_frame_skip_padding(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    if data.is_empty() {
        return None;
    }

    if let Some(result) = packet::decode_tcp_frame(data) {
        return Some(result);
    }

    for offset in 1..data.len() {
        if let Some((payload, consumed)) = packet::decode_tcp_frame(&data[offset..]) {
            return Some((payload, offset + consumed));
        }
    }

    None
}
