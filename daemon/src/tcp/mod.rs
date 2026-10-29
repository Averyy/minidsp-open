//! TCP bridge server (port 5333) — frame-level forwarding for Device Console
//! compatibility.
//!
//! Requests are reassembled independently of socket read boundaries. Validated
//! 64-byte padded requests enable Wi-DG response padding; ordinary coalesced
//! frames remain length-prefixed. Invalid complete frames are skipped, while
//! incomplete frames wait for more bytes.
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

    loop {
        // Try to decode frames from buffered data first.
        loop {
            let padded_request = is_padded_tcp_chunk(&read_buf);
            chunked_mode |= padded_request;
            let decoded = decode_tcp_frame_skip_padding(&read_buf);
            match decoded {
                Some((payload, consumed)) => {
                    read_buf.drain(..if padded_request { 64 } else { consumed });

                    if payload.is_empty() {
                        continue;
                    }
                    if let Some(client) = state.get_device(0).await {
                        let tcp_request = payload.clone();
                        let result = {
                            let _guard = bridge_mutex.lock().await;
                            crate::bridge::execute_raw_bridge_command(&state, &client, payload)
                                .await
                        };
                        match result {
                            Ok(raw_resp) => {
                                let frame = if chunked_mode {
                                    let mut frame = packet::encode_frame(&raw_resp);
                                    frame.resize(64.max(frame.len()), 0);
                                    frame
                                } else {
                                    packet::encode_device_console_tcp_response(
                                        &tcp_request,
                                        &raw_resp,
                                        &client.profile().dialect,
                                    )
                                };
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

        let n = match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::warn!("TCP bridge read error ({peer}): {e}");
                break;
            }
        };

        if read_buf.len() + n > 1024 {
            tracing::warn!("TCP bridge ({peer}): request buffer limit exceeded");
            return;
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

const MAX_HID_FRAME_LEN: usize = 64;

/// Recognize padding by frame shape, never by the size of a socket read.
fn is_padded_tcp_chunk(data: &[u8]) -> bool {
    if data.len() < 64 {
        return false;
    }
    decode_tcp_frame_skip_padding(&data[..64]).is_some_and(|(payload, consumed)| {
        !payload.is_empty()
            && consumed < 64
            && data[consumed..64]
                .iter()
                .all(|&byte| byte == 0 || byte == 0xff)
    })
}

/// Empty payloads indicate discarded padding. An incomplete candidate must
/// stay buffered so a checksum-valid sequence inside its payload cannot run.
fn decode_tcp_frame_skip_padding(data: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut offset = 0;
    while offset < data.len() {
        let len = data[offset] as usize;
        // A request frame (length + payload + checksum) must fit one 64-byte
        // HID report, so any other length byte is padding (0x00 or 0xFF), not
        // a frame start worth waiting for.
        if !(2..MAX_HID_FRAME_LEN).contains(&len) {
            offset += 1;
            continue;
        }
        if data.len() - offset < len + 1 {
            return (offset > 0).then(|| (vec![], offset));
        }
        if let Some((payload, consumed)) = packet::decode_tcp_frame(&data[offset..]) {
            return Some((payload, offset + consumed));
        }
        offset += 1;
    }
    (offset > 0).then(|| (vec![], offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesced_frames_are_not_mistaken_for_padded_chunks() {
        let mut data = packet::encode_frame(&[0x31]).repeat(22);
        assert_eq!(data.len(), 66);
        assert!(!is_padded_tcp_chunk(&data));
        for _ in 0..22 {
            let (payload, consumed) = decode_tcp_frame_skip_padding(&data).unwrap();
            assert_eq!(payload, vec![0x31]);
            data.drain(..consumed);
        }
        assert!(data.is_empty());
    }

    #[test]
    fn padded_chunks_and_unknown_commands_preserve_frame_boundaries() {
        let mut data = packet::encode_frame(&[0x31]);
        data.resize(64, 0);
        data.extend_from_slice(&packet::encode_frame(&[0xe9, 4, 3, 2]));
        assert!(is_padded_tcp_chunk(&data));
        let (payload, consumed) = decode_tcp_frame_skip_padding(&data).unwrap();
        assert_eq!(payload, vec![0x31]);
        data.drain(..consumed);
        assert_eq!(
            decode_tcp_frame_skip_padding(&data),
            Some((vec![0xe9, 4, 3, 2], data.len()))
        );
    }

    #[test]
    fn padded_chunk_can_have_a_leading_zero_report_id() {
        let mut data = vec![0];
        data.extend_from_slice(&packet::encode_frame(&[0x31]));
        data.resize(64, 0xff);
        assert!(is_padded_tcp_chunk(&data));
        assert_eq!(decode_tcp_frame_skip_padding(&data), Some((vec![0x31], 4)));
    }

    #[test]
    fn padding_stream_is_consumed_without_growing_buffer() {
        for padding in [0, 0xff] {
            let mut data = Vec::new();
            for _ in 0..100 {
                data.extend_from_slice(&[padding; 512]);
                let (payload, consumed) = decode_tcp_frame_skip_padding(&data).unwrap();
                assert!(payload.is_empty());
                data.drain(..consumed);
                assert!(data.is_empty());
            }
        }
    }

    #[test]
    fn fragmented_frame_waits_at_every_boundary() {
        let data = packet::encode_frame(&[0xe9, 4, 3, 2, 0x31, 0x33]);
        for split in 1..data.len() {
            assert_eq!(decode_tcp_frame_skip_padding(&data[..split]), None);
        }
        assert_eq!(
            decode_tcp_frame_skip_padding(&data).unwrap().0,
            data[1..data.len() - 1]
        );
    }

    #[test]
    fn maximum_length_hid_frame_waits_for_all_bytes() {
        let payload = vec![0xe9; 62];
        let data = packet::encode_frame(&payload);
        assert_eq!(data.len(), 64);
        for split in 1..data.len() {
            assert_eq!(decode_tcp_frame_skip_padding(&data[..split]), None);
        }
        assert_eq!(decode_tcp_frame_skip_padding(&data), Some((payload, 64)));
    }

    #[test]
    fn short_ff_padding_ahead_of_a_frame_does_not_stall() {
        let mut data = vec![0xff; 20];
        data.extend_from_slice(&packet::encode_frame(&[0x31]));
        assert_eq!(
            decode_tcp_frame_skip_padding(&data),
            Some((vec![0x31], data.len()))
        );
        // A lone run of 0xFF padding is consumed rather than awaited.
        data.truncate(20);
        assert_eq!(decode_tcp_frame_skip_padding(&data), Some((vec![], 20)));
    }

    #[test]
    fn incomplete_frame_does_not_execute_embedded_frame() {
        let data = [0x07, 0x04, 0xff, 0xc8, 0x02, 0x31, 0x33, 0x38];
        assert_eq!(decode_tcp_frame_skip_padding(&data[..7]), None);
        assert_eq!(
            decode_tcp_frame_skip_padding(&data),
            Some((vec![0x04, 0xff, 0xc8, 0x02, 0x31, 0x33], 8))
        );
    }
}
