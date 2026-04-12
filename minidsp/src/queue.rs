//! Command queue with priority, batch execution, and level polling pause.

use crate::reconnect::{ReconnectConfig, backoff_delay};
use crate::transport::{Transport, TransportError};
use crate::transport::{hid::HidTransport, tcp::TcpTransport, ws::WsTransport};
use minidsp_protocol::commands::{self, Response};
use minidsp_protocol::dialect::Dialect;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify, oneshot};

/// Command priority levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Telemetry reads (level meters). Droppable.
    Low = 0,
    /// Status reads. Should complete but can wait.
    Normal = 1,
    /// Config writes. User-initiated, must not be starved.
    High = 2,
    /// State transitions. Must complete atomically.
    Critical = 3,
}

/// Response with both parsed and raw bytes.
pub struct QueueResponse {
    pub parsed: Response,
    pub raw: Vec<u8>,
}

struct QueuedCommand {
    payload: Vec<u8>,
    #[allow(dead_code)]
    priority: Priority,
    response_tx: oneshot::Sender<Result<QueueResponse, TransportError>>,
    allow_unknown_response: bool,
    #[allow(dead_code)]
    batch_id: Option<u64>,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum QueueState {
    Idle,
    AwaitingResponse,
    DeviceBusy,
}

/// Maximum queue depth before returning a busy error.
pub const MAX_QUEUE_DEPTH: usize = 64;
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_SLOW_COMMAND_TIMEOUT: Duration = Duration::from_secs(35);

fn default_timeout_for_payload(payload: &[u8]) -> Duration {
    match payload.first().copied() {
        Some(commands::CMD_COPY_PRESET | commands::CMD_RESET) => DEFAULT_SLOW_COMMAND_TIMEOUT,
        _ => DEFAULT_COMMAND_TIMEOUT,
    }
}

/// Per-device command queue with priority scheduling.
pub struct CommandQueue {
    transport: Mutex<Box<dyn Transport>>,
    transport_url: String,
    reconnect_config: ReconnectConfig,
    queues: Mutex<[VecDeque<QueuedCommand>; 4]>,
    #[allow(dead_code)]
    state: Mutex<QueueState>,
    notify: Notify,
    levels_paused: std::sync::atomic::AtomicBool,
    next_batch_id: std::sync::atomic::AtomicU64,
    // We default to new dialect for parsing responses; the client overrides this
    dialect: Mutex<Dialect>,
    reconnect_epoch: std::sync::atomic::AtomicU64,
    processing: Mutex<()>,
}

impl CommandQueue {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self::with_reconnect(transport, ReconnectConfig::default())
    }

    pub fn with_reconnect(
        transport: Box<dyn Transport>,
        reconnect_config: ReconnectConfig,
    ) -> Self {
        let transport_url = transport.url();
        Self {
            transport: Mutex::new(transport),
            transport_url,
            reconnect_config,
            queues: Mutex::new([
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
                VecDeque::new(),
            ]),
            state: Mutex::new(QueueState::Idle),
            notify: Notify::new(),
            levels_paused: std::sync::atomic::AtomicBool::new(false),
            next_batch_id: std::sync::atomic::AtomicU64::new(1),
            dialect: Mutex::new(Dialect::NEW),
            reconnect_epoch: std::sync::atomic::AtomicU64::new(0),
            processing: Mutex::new(()),
        }
    }

    pub async fn set_dialect(&self, dialect: Dialect) {
        *self.dialect.lock().await = dialect;
    }

    /// Returns the total number of queued commands across all priority levels.
    pub async fn queue_depth(&self) -> usize {
        let queues = self.queues.lock().await;
        queues.iter().map(|q| q.len()).sum()
    }

    /// Execute a single command and return the parsed response.
    /// Returns `TransportError::Busy` if the queue is full.
    pub async fn execute(
        &self,
        payload: Vec<u8>,
        priority: Priority,
    ) -> Result<Response, TransportError> {
        let timeout = default_timeout_for_payload(&payload);
        let qr = self
            .execute_inner(payload, priority, false, timeout)
            .await?;
        Ok(qr.parsed)
    }

    /// Execute a single command with a custom timeout and return the parsed response.
    pub async fn execute_with_timeout(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        timeout: Duration,
    ) -> Result<Response, TransportError> {
        let qr = self
            .execute_inner(payload, priority, false, timeout)
            .await?;
        Ok(qr.parsed)
    }

    /// Execute a command and return raw response bytes (for TCP bridge passthrough).
    pub async fn execute_raw(
        &self,
        payload: Vec<u8>,
        priority: Priority,
    ) -> Result<Vec<u8>, TransportError> {
        let timeout = default_timeout_for_payload(&payload);
        let qr = self.execute_inner(payload, priority, true, timeout).await?;
        Ok(qr.raw)
    }

    /// Execute a command with a custom timeout and return raw response bytes.
    pub async fn execute_raw_with_timeout(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        let qr = self.execute_inner(payload, priority, true, timeout).await?;
        Ok(qr.raw)
    }

    async fn execute_inner(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        allow_unknown_response: bool,
        timeout: Duration,
    ) -> Result<QueueResponse, TransportError> {
        // Check queue depth before enqueuing
        {
            let queues = self.queues.lock().await;
            let depth: usize = queues.iter().map(|q| q.len()).sum();
            if depth >= MAX_QUEUE_DEPTH {
                return Err(TransportError::Busy);
            }
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut queues = self.queues.lock().await;
            queues[priority as usize].push_back(QueuedCommand {
                payload,
                priority,
                response_tx: tx,
                allow_unknown_response,
                batch_id: None,
                timeout,
            });
        }
        self.notify.notify_one();
        self.process_next().await;
        rx.await
            .map_err(|_| TransportError::Io("response channel closed".into()))?
    }

    /// Submit a batch of commands that execute contiguously.
    pub async fn execute_batch(
        &self,
        commands: Vec<Vec<u8>>,
        priority: Priority,
    ) -> Result<Vec<Response>, TransportError> {
        let batch_id = self
            .next_batch_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut receivers = Vec::with_capacity(commands.len());

        {
            let mut queues = self.queues.lock().await;
            for payload in commands {
                let (tx, rx) = oneshot::channel();
                queues[priority as usize].push_back(QueuedCommand {
                    payload,
                    priority,
                    response_tx: tx,
                    allow_unknown_response: false,
                    batch_id: Some(batch_id),
                    timeout: Duration::from_secs(30),
                });
                receivers.push(rx);
            }
        }

        // Pause level polling during batch
        self.levels_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.notify.notify_one();

        // Process all queued commands
        for _ in 0..receivers.len() {
            self.process_next().await;
        }

        self.levels_paused
            .store(false, std::sync::atomic::Ordering::Relaxed);

        let mut responses = Vec::with_capacity(receivers.len());
        for rx in receivers {
            let qr = rx
                .await
                .map_err(|_| TransportError::Io("response channel closed".into()))??;
            responses.push(qr.parsed);
        }
        Ok(responses)
    }

    /// Check if level polling is paused.
    pub fn is_levels_paused(&self) -> bool {
        self.levels_paused
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn pause_levels(&self) {
        self.levels_paused
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn resume_levels(&self) {
        self.levels_paused
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn reconnect_epoch(&self) -> u64 {
        self.reconnect_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Process the next command in the queue.
    async fn process_next(&self) {
        let _processing = self.processing.lock().await;
        let cmd = {
            let mut queues = self.queues.lock().await;
            // Pick highest-priority non-empty queue
            let cmd = (0..4).rev().find_map(|p| queues[p].pop_front());
            cmd
        };

        let Some(cmd) = cmd else { return };

        loop {
            match self.execute_queued_command(&cmd).await {
                Ok(response) => {
                    let _ = cmd.response_tx.send(Ok(response));
                    return;
                }
                Err(TransportError::Disconnected) => {
                    if let Err(e) = self.reconnect_transport().await {
                        let _ = cmd.response_tx.send(Err(e));
                        return;
                    }
                }
                Err(e) => {
                    let _ = cmd.response_tx.send(Err(e));
                    return;
                }
            }
        }
    }

    async fn execute_queued_command(
        &self,
        cmd: &QueuedCommand,
    ) -> Result<QueueResponse, TransportError> {
        let transport = self.transport.lock().await;
        let deadline = Instant::now() + cmd.timeout;
        let request_opcode = cmd
            .payload
            .first()
            .copied()
            .ok_or_else(|| TransportError::Io("empty command payload".into()))?;

        transport.send_frame(&cmd.payload).await?;

        loop {
            let mut raw = match transport
                .recv_frame(remaining_timeout_ms(deadline)?.min(500))
                .await
            {
                Ok(raw) => raw,
                Err(TransportError::Timeout) => continue,
                Err(err) => return Err(err),
            };
            // Trim HID echo garbage: for write commands, real DSP100/2x4 HD hardware
            // returns an unframed 0x01-prefixed echo of the full 64-byte HID buffer.
            // decode_hid_response strips the 0x01 prefix but returns all 63 trailing
            // bytes including garbage. Trim to the original request length so TCP
            // bridge clients (Device Console, minidsp-rs) only see the echoed command.
            if commands::is_write_command(request_opcode)
                && raw.first().copied() == cmd.payload.first().copied()
                && raw.len() > cmd.payload.len()
            {
                raw.truncate(cmd.payload.len());
            }
            let response = {
                let dialect = self.dialect.lock().await;
                commands::parse_response_for_request(&cmd.payload, &raw, &dialect)
            };
            let response_opcode = raw.first().copied().unwrap_or(commands::RESP_OK);

            if !commands::matches_response(request_opcode, response_opcode) {
                tracing::debug!(
                    "queue: skipping stale response {:02x?} for request {:02x?}",
                    raw,
                    cmd.payload
                );
                continue;
            }

            if matches!(response, Response::Unknown(_))
                && request_opcode == commands::CMD_READ_FLOATS
            {
                let matches_typed_read = {
                    let dialect = self.dialect.lock().await;
                    match dialect.value_format {
                        minidsp_protocol::dialect::ValueFormat::Float32 => {
                            commands::parse_param_words_response_for_request(&cmd.payload, &raw)
                                .is_some()
                        }
                        minidsp_protocol::dialect::ValueFormat::FixedPoint => {
                            commands::parse_param_words_be_response_for_request(&cmd.payload, &raw)
                                .is_some()
                        }
                    }
                };

                if !matches_typed_read {
                    tracing::debug!(
                        "queue: skipping unknown readfloats response {:02x?} for request {:02x?}",
                        raw,
                        cmd.payload
                    );
                    continue;
                }
            }

            if matches!(response, Response::Unknown(_)) && !cmd.allow_unknown_response {
                tracing::debug!(
                    "queue: skipping unknown request-specific response {:02x?} for request {:02x?}",
                    raw,
                    cmd.payload
                );
                continue;
            }

            return Ok(QueueResponse {
                parsed: response,
                raw,
            });
        }
    }

    async fn reconnect_transport(&self) -> Result<(), TransportError> {
        let mut attempt = 0u32;

        loop {
            if self.reconnect_config.max_retries != 0
                && attempt >= self.reconnect_config.max_retries
            {
                return Err(TransportError::Disconnected);
            }

            let delay = backoff_delay(&self.reconnect_config, attempt);
            tracing::warn!(
                "transport disconnected; reconnecting via {} in {:?} (attempt {})",
                self.transport_url,
                delay,
                attempt + 1,
            );
            tokio::time::sleep(delay).await;

            match open_transport(&self.transport_url).await {
                Ok(new_transport) => {
                    let mut transport = self.transport.lock().await;
                    *transport = new_transport;
                    self.reconnect_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!("transport reconnected via {}", self.transport_url);
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("transport reconnect attempt {} failed: {}", attempt + 1, e);
                    attempt += 1;
                }
            }
        }
    }
}

fn remaining_timeout_ms(deadline: Instant) -> Result<u64, TransportError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(TransportError::Timeout);
    }

    Ok(remaining
        .as_millis()
        .clamp(1, u64::MAX as u128)
        .try_into()
        .unwrap_or(u64::MAX))
}

async fn open_transport(url: &str) -> Result<Box<dyn Transport>, TransportError> {
    if let Some(addr) = url.strip_prefix("tcp://") {
        return Ok(Box::new(TcpTransport::connect(addr).await?));
    }

    if url.starts_with("ws://") || url.starts_with("http://") {
        return Ok(Box::new(WsTransport::connect(url, 0).await?));
    }

    if let Some(rest) = url.strip_prefix("ws+unix:") {
        let (path, device_id) = parse_ws_unix_url(rest)?;
        #[cfg(unix)]
        {
            return Ok(Box::new(WsTransport::connect_unix(&path, device_id).await?));
        }
        #[cfg(not(unix))]
        {
            let _ = (path, device_id);
            return Err(TransportError::Io(
                "ws+unix reconnect is only supported on unix".into(),
            ));
        }
    }

    if let Some(rest) = url.strip_prefix("usb:") {
        let usb = parse_usb_url(rest)?;
        match HidTransport::open(&usb.path) {
            Ok(transport) => return Ok(Box::new(transport)),
            Err(path_err) => {
                tracing::warn!(
                    "failed to reopen HID device at {}; re-enumerating vid={:04x} pid={:04x}: {}",
                    usb.path,
                    usb.vid,
                    usb.pid,
                    path_err
                );
                let devices = crate::transport::hid::enumerate()?;
                if let Some(device) = devices
                    .into_iter()
                    .find(|device| device.vendor_id == usb.vid && device.product_id == usb.pid)
                {
                    return Ok(Box::new(HidTransport::open(&device.path)?));
                }
                return Err(path_err);
            }
        }
    }

    Err(TransportError::Io(format!(
        "unsupported transport url for reconnect: {url}"
    )))
}

fn parse_ws_unix_url(rest: &str) -> Result<(String, usize), TransportError> {
    let (path, suffix) = rest
        .rsplit_once("/devices/")
        .ok_or_else(|| TransportError::Io(format!("invalid ws+unix transport url: {rest}")))?;
    let device_id = suffix
        .strip_suffix("/ws")
        .ok_or_else(|| TransportError::Io(format!("invalid ws+unix transport url: {rest}")))?
        .parse::<usize>()
        .map_err(|e| TransportError::Io(format!("invalid ws+unix device id: {e}")))?;
    Ok((path.to_string(), device_id))
}

struct ParsedUsbUrl {
    path: String,
    vid: u16,
    pid: u16,
}

fn parse_usb_url(rest: &str) -> Result<ParsedUsbUrl, TransportError> {
    let (encoded_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = urlencoding::decode(encoded_path)
        .map_err(|e| TransportError::Io(format!("invalid usb path encoding: {e}")))?;

    let mut vid = None;
    let mut pid = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "vid" => {
                vid = Some(
                    u16::from_str_radix(value, 16)
                        .map_err(|e| TransportError::Io(format!("invalid usb vid: {e}")))?,
                );
            }
            "pid" => {
                pid = Some(
                    u16::from_str_radix(value, 16)
                        .map_err(|e| TransportError::Io(format!("invalid usb pid: {e}")))?,
                );
            }
            _ => {}
        }
    }

    Ok(ParsedUsbUrl {
        path: path.into_owned(),
        vid: vid.ok_or_else(|| TransportError::Io("missing usb vid in transport url".into()))?,
        pid: pid.ok_or_else(|| TransportError::Io("missing usb pid in transport url".into()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidsp_protocol::commands;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    struct FakeTransport {
        sent: Arc<StdMutex<Vec<Vec<u8>>>>,
        responses: Arc<StdMutex<VecDeque<Result<Vec<u8>, TransportError>>>>,
        recv_calls: Arc<AtomicUsize>,
    }

    impl FakeTransport {
        fn new(responses: impl IntoIterator<Item = Result<Vec<u8>, TransportError>>) -> Self {
            Self {
                sent: Arc::new(StdMutex::new(Vec::new())),
                responses: Arc::new(StdMutex::new(responses.into_iter().collect())),
                recv_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Transport for FakeTransport {
        fn send_frame(
            &self,
            payload: &[u8],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>,
        > {
            let sent = self.sent.clone();
            let payload = payload.to_vec();
            Box::pin(async move {
                sent.lock().unwrap().push(payload);
                Ok(())
            })
        }

        fn recv_frame(
            &self,
            _timeout_ms: u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>,
        > {
            let responses = self.responses.clone();
            let recv_calls = self.recv_calls.clone();
            Box::pin(async move {
                recv_calls.fetch_add(1, Ordering::Relaxed);
                responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Err(TransportError::Timeout))
            })
        }

        fn is_connected(&self) -> bool {
            true
        }

        fn close(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>,
        > {
            Box::pin(async { Ok(()) })
        }

        fn url(&self) -> String {
            "fake://queue-test".to_string()
        }
    }

    #[tokio::test]
    async fn set_config_returns_first_ack_without_waiting_for_second_frame() {
        let transport = FakeTransport::new([Ok(vec![commands::CMD_SET_CONFIG])]);
        let recv_calls = transport.recv_calls.clone();
        let queue = CommandQueue::new(Box::new(transport));

        let resp = queue
            .execute(
                commands::set_config(1, commands::PRESET_CHANGE_NORMAL),
                Priority::Critical,
            )
            .await
            .expect("set_config should accept opcode ack");

        assert!(matches!(resp, Response::Ack));
        assert_eq!(recv_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn set_config_accepts_preset_complete_frame() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(vec![
            commands::RESP_PRESET_COMPLETE,
        ])])));

        let resp = queue
            .execute(
                commands::set_config(2, commands::PRESET_CHANGE_NORMAL),
                Priority::Critical,
            )
            .await
            .expect("set_config should accept preset complete");

        assert!(matches!(resp, Response::PresetComplete));
    }

    #[tokio::test]
    async fn set_source_skips_stale_flash_response_and_accepts_ack() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([
            Ok(vec![
                commands::CMD_READ_FLASH,
                0xFF,
                0xD8,
                0x01,
                0x00,
                0x00,
                0x00,
            ]),
            Ok(vec![commands::CMD_SET_SOURCE, 0x02]),
        ])));

        let resp = queue
            .execute(commands::set_source(2), Priority::High)
            .await
            .expect("set_source should ignore stale flash data");

        assert!(matches!(resp, Response::Ack));
    }

    #[tokio::test]
    async fn read_after_source_change_skips_official_stale_flash_notification() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([
            Ok(vec![commands::CMD_SET_SOURCE, 0x02, 0x4a, 0x00, 0x00, 0x00]),
            Ok(vec![commands::CMD_READ_FLASH, 0xff, 0xa9, 0x02]),
            Ok(vec![
                commands::CMD_READ_FLOATS,
                0x00,
                0x44,
                0xd0,
                0x8f,
                0x00,
                0xc3,
                0xd0,
                0x8f,
                0x00,
                0xc3,
            ]),
        ])));

        let set_source_resp = queue
            .execute(commands::set_source(2), Priority::High)
            .await
            .expect("set_source should accept official opcode echo");
        assert!(matches!(set_source_resp, Response::Ack));

        let read_resp = queue
            .execute(
                commands::read_floats(0x0044, 2, &Dialect::NEW),
                Priority::Normal,
            )
            .await
            .expect("next read should skip stale source flash notification");

        match read_resp {
            Response::Floats(values) => {
                assert_eq!(values.len(), 2);
                let expected = f32::from_le_bytes([0xd0, 0x8f, 0x00, 0xc3]);
                assert_eq!(values[0].to_bits(), expected.to_bits());
                assert_eq!(values[1].to_bits(), expected.to_bits());
            }
            other => panic!("expected floats after stale flash skip, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_flash_accepts_official_opcode_echo_ack() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(vec![
            commands::CMD_WRITE_FLASH,
            0xff,
            0xc8,
            0x5a,
            0x4c,
            0x1b,
            0x2b,
        ])])));

        let resp = queue
            .execute(
                commands::write_flash(0xff, 0xc8, &[0x5a, 0x4c, 0x1b, 0x2b]),
                Priority::High,
            )
            .await
            .expect("write_flash should accept official opcode echo ack");

        assert!(matches!(resp, Response::Ack));
    }

    #[tokio::test]
    async fn execute_raw_returns_matching_unknown_readfloats_frame() {
        let raw = vec![
            commands::CMD_READ_FLOATS,
            0x00,
            0x16,
            0x01,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
        ];
        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(raw.clone())])));

        let resp = queue
            .execute_raw(
                commands::read_floats(0x0016, 2, &Dialect::NEW),
                Priority::Normal,
            )
            .await
            .expect("execute_raw should return matching raw frame even if float parser rejects it");

        assert_eq!(resp, raw);
    }

    #[tokio::test]
    async fn execute_raw_skips_wrong_address_unknown_readfloats_frame() {
        let stale = vec![
            commands::CMD_READ_FLOATS,
            0x00,
            0x4a,
            0x08,
            0x99,
            0x03,
            0x4a,
            0xd3,
            0xdc,
            0x42,
            0xfd,
        ];
        let expected = vec![
            commands::CMD_READ_FLOATS,
            0x00,
            0x16,
            0x01,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
        ];
        let queue = CommandQueue::new(Box::new(FakeTransport::new([
            Ok(stale),
            Ok(expected.clone()),
        ])));

        let resp = queue
            .execute_raw(
                commands::read_floats(0x0016, 2, &Dialect::NEW),
                Priority::Normal,
            )
            .await
            .expect("execute_raw should skip stale unknown readfloats frames that do not match the request");

        assert_eq!(resp, expected);
    }

    #[tokio::test]
    async fn execute_retries_after_recv_timeout_until_ack_arrives() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([
            Err(TransportError::Timeout),
            Ok(vec![commands::RESP_OK]),
        ])));

        let resp = queue
            .execute(commands::set_source(2), Priority::High)
            .await
            .expect("queue should keep polling until ack arrives before the deadline");

        assert!(matches!(resp, Response::Ack));
    }

    #[tokio::test]
    async fn queued_commands_start_timeout_when_dispatched() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([
            Ok(vec![commands::RESP_OK]),
            Ok(vec![commands::RESP_OK]),
        ])));
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        {
            let mut queues = queue.queues.lock().await;
            queues[Priority::High as usize].push_back(QueuedCommand {
                payload: commands::set_source(1),
                priority: Priority::High,
                response_tx: tx1,
                allow_unknown_response: false,
                batch_id: None,
                timeout: Duration::from_millis(5),
            });
            queues[Priority::High as usize].push_back(QueuedCommand {
                payload: commands::set_source(2),
                priority: Priority::High,
                response_tx: tx2,
                allow_unknown_response: false,
                batch_id: None,
                timeout: Duration::from_millis(5),
            });
        }

        std::thread::sleep(Duration::from_millis(20));

        queue.process_next().await;
        queue.process_next().await;

        let first = rx1.await.expect("first response channel");
        let second = rx2.await.expect("second response channel");
        assert!(matches!(
            first.expect("first response"),
            QueueResponse {
                parsed: Response::Ack,
                ..
            }
        ));
        assert!(matches!(
            second.expect("second response"),
            QueueResponse {
                parsed: Response::Ack,
                ..
            }
        ));
    }

    #[test]
    fn copy_preset_uses_extended_default_timeout() {
        assert_eq!(
            default_timeout_for_payload(&commands::copy_preset(true)),
            DEFAULT_SLOW_COMMAND_TIMEOUT
        );
    }

    #[test]
    fn reset_uses_extended_default_timeout() {
        assert_eq!(
            default_timeout_for_payload(&commands::reset()),
            DEFAULT_SLOW_COMMAND_TIMEOUT
        );
    }

    #[test]
    fn regular_commands_keep_standard_default_timeout() {
        assert_eq!(
            default_timeout_for_payload(&commands::set_source(2)),
            DEFAULT_COMMAND_TIMEOUT
        );
    }

    /// Regression test: WriteDSP response from real 2x4 HD hardware arrives as
    /// a 63-byte buffer (0x01-prefix stripped by decode_hid_response, 63-1=62
    /// bytes of payload). The queue must trim that down to the 8-byte echoed
    /// command so TCP bridge clients see `0x13 0x80 0x00 0x16 0x02 0x00 0x00 0x00`
    /// instead of `0x13 0x80 ... <54 bytes of garbage>`.
    #[tokio::test]
    async fn write_dsp_response_trimmed_to_request_length() {
        // Simulate the raw response as returned by decode_hid_response for a
        // 0x01-prefixed HID echo: first 8 bytes are the echoed command, the
        // rest are HID buffer garbage from real hardware.
        let request = vec![0x13u8, 0x80, 0x00, 0x16, 0x02, 0x00, 0x00, 0x00]; // WriteDSP
        let mut oversized_response = request.clone();
        // Append 55 bytes of garbage (0x01-prefix stripped, 63-byte buffer → 63 bytes;
        // but we test the trim logic directly, so just append enough garbage).
        oversized_response.extend(std::iter::repeat(0xed).take(55));
        assert_eq!(oversized_response.len(), 63);

        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(oversized_response)])));

        let resp = queue
            .execute_raw(request.clone(), Priority::High)
            .await
            .expect("execute_raw should succeed with oversized write echo");

        // The raw returned to the TCP bridge must be exactly the echoed command, not 63 bytes.
        assert_eq!(
            resp.len(),
            request.len(),
            "raw response should be trimmed to request length ({}), got {} bytes: {:02x?}",
            request.len(),
            resp.len(),
            resp
        );
        assert_eq!(
            &resp[..],
            &request[..],
            "trimmed response should equal the echoed command"
        );
    }

    /// Confirm read commands (0x14 ReadFloats) are NOT trimmed even if the
    /// response happens to start with the same opcode byte.
    #[tokio::test]
    async fn read_floats_response_not_trimmed() {
        let request = commands::read_floats(0x0016, 2, &minidsp_protocol::dialect::Dialect::NEW);
        // A real ReadFloats response: opcode + addr (2) + count (1) + values (8) = 12 bytes
        let response = vec![
            commands::CMD_READ_FLOATS,
            0x00,
            0x16,
            0x01,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
        ];
        let expected_len = response.len();
        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(response)])));

        let resp = queue
            .execute_raw(request.clone(), Priority::Normal)
            .await
            .expect("execute_raw should succeed for read floats");

        assert_eq!(
            resp.len(),
            expected_len,
            "read floats response must not be trimmed: got {} bytes",
            resp.len()
        );
    }
}
