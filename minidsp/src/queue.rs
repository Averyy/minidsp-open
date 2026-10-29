//! Command queue with priority, batch execution, and level polling pause.

use crate::reconnect::{ReconnectConfig, backoff_delay};
use crate::transport::{Transport, TransportError};
use crate::transport::{hid::HidTransport, tcp::TcpTransport, ws::WsTransport};
use minidsp_protocol::commands::{self, Response};
use minidsp_protocol::dialect::Dialect;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, oneshot, watch};

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
    replay_safe: bool,
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
        Some(
            commands::CMD_COPY_PRESET
            | commands::CMD_RESET
            | commands::CMD_WRITE_FLASH_BLOCK
            | commands::CMD_FINISH_FLASH_BLOCK,
        ) => DEFAULT_SLOW_COMMAND_TIMEOUT,
        _ => DEFAULT_COMMAND_TIMEOUT,
    }
}

/// Per-device command queue with priority scheduling.
#[derive(Clone)]
pub struct CommandQueue {
    inner: Arc<QueueInner>,
    // Executors hold only inner, so the last external handle closes this channel.
    shutdown_tx: watch::Sender<bool>,
}

struct QueueInner {
    transport: Mutex<Box<dyn Transport>>,
    transport_url: String,
    reconnect_config: ReconnectConfig,
    queues: Mutex<[VecDeque<QueuedCommand>; 4]>,
    #[allow(dead_code)]
    state: Mutex<QueueState>,
    // Outstanding `LevelsPause` guards; polling resumes when this is zero.
    levels_pauses: std::sync::atomic::AtomicUsize,
    next_batch_id: std::sync::atomic::AtomicU64,
    // We default to new dialect for parsing responses; the client overrides this
    dialect: Mutex<Dialect>,
    reconnect_epoch: std::sync::atomic::AtomicU64,
    processing: Mutex<()>,
    needs_reconnect: std::sync::atomic::AtomicBool,
    batch_active: std::sync::atomic::AtomicBool,
    disconnect_epoch: std::sync::atomic::AtomicU64,
    shutdown_rx: watch::Receiver<bool>,
}

/// Keeps level polling paused while alive; see `CommandQueue::pause_levels`.
pub struct LevelsPause {
    queue: CommandQueue,
}

impl Drop for LevelsPause {
    fn drop(&mut self) {
        self.queue
            .inner
            .levels_pauses
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
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
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            inner: Arc::new(QueueInner {
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
                levels_pauses: std::sync::atomic::AtomicUsize::new(0),
                next_batch_id: std::sync::atomic::AtomicU64::new(1),
                dialect: Mutex::new(Dialect::NEW),
                reconnect_epoch: std::sync::atomic::AtomicU64::new(0),
                processing: Mutex::new(()),
                needs_reconnect: std::sync::atomic::AtomicBool::new(false),
                batch_active: std::sync::atomic::AtomicBool::new(false),
                disconnect_epoch: std::sync::atomic::AtomicU64::new(0),
                shutdown_rx,
            }),
            shutdown_tx,
        }
    }

    pub async fn set_dialect(&self, dialect: Dialect) {
        *self.inner.dialect.lock().await = dialect;
    }

    pub async fn close(&self) -> Result<(), TransportError> {
        self.shutdown_tx.send_replace(true);
        let _processing = self.inner.processing.lock().await;
        let transport = self.inner.transport.lock().await;
        transport.close().await
    }

    /// Returns the total number of queued commands across all priority levels.
    pub async fn queue_depth(&self) -> usize {
        let queues = self.inner.queues.lock().await;
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
            .execute_inner(payload, priority, false, true, timeout)
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
            .execute_inner(payload, priority, false, true, timeout)
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
        let qr = self
            .execute_inner(payload, priority, true, false, timeout)
            .await?;
        Ok(qr.raw)
    }

    /// Execute a command with a custom timeout and return raw response bytes.
    pub async fn execute_raw_with_timeout(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        let qr = self
            .execute_inner(payload, priority, true, false, timeout)
            .await?;
        Ok(qr.raw)
    }

    pub(crate) async fn execute_typed_raw_with_timeout(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransportError> {
        let qr = self
            .execute_inner(payload, priority, true, true, timeout)
            .await?;
        Ok(qr.raw)
    }

    async fn execute_inner(
        &self,
        payload: Vec<u8>,
        priority: Priority,
        allow_unknown_response: bool,
        allow_replay: bool,
        timeout: Duration,
    ) -> Result<QueueResponse, TransportError> {
        // Check queue depth before enqueuing
        {
            let queues = self.inner.queues.lock().await;
            let depth: usize = queues.iter().map(|q| q.len()).sum();
            if depth >= MAX_QUEUE_DEPTH {
                return Err(TransportError::Busy);
            }
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut queues = self.inner.queues.lock().await;
            if self.inner.is_shutdown() {
                return Err(TransportError::Disconnected);
            }
            queues[priority as usize].push_back(QueuedCommand {
                replay_safe: allow_replay && is_replay_safe(&payload),
                payload,
                priority,
                response_tx: tx,
                allow_unknown_response,
                batch_id: None,
                timeout,
            });
        }
        self.start_executor();
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
            .inner
            .next_batch_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut receivers = Vec::with_capacity(commands.len());

        {
            let mut queues = self.inner.queues.lock().await;
            if self.inner.is_shutdown() {
                return Err(TransportError::Disconnected);
            }
            for payload in commands {
                let (tx, rx) = oneshot::channel();
                queues[priority as usize].push_back(QueuedCommand {
                    replay_safe: is_replay_safe(&payload),
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

        self.start_executor();

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
        self.inner
            .levels_pauses
            .load(std::sync::atomic::Ordering::Relaxed)
            != 0
            || self
                .inner
                .batch_active
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Pause level polling until the returned guard drops. Guards nest, and
    /// dropping one on cancellation still resumes polling.
    pub fn pause_levels(&self) -> LevelsPause {
        self.inner
            .levels_pauses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        LevelsPause {
            queue: self.clone(),
        }
    }

    pub fn reconnect_epoch(&self) -> u64 {
        self.inner
            .reconnect_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn disconnect_epoch(&self) -> u64 {
        self.inner
            .disconnect_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn start_executor(&self) {
        let queue = self.inner.clone();
        tokio::spawn(async move { queue.process_next().await });
    }
}

impl QueueInner {
    fn is_shutdown(&self) -> bool {
        *self.shutdown_rx.borrow() || self.shutdown_rx.has_changed().is_err()
    }

    /// Drain independently of callers so cancellation cannot abandon an exchange.
    async fn process_next(&self) {
        let _processing = self.processing.lock().await;
        let mut shutdown = self.shutdown_rx.clone();
        if !self.is_shutdown() {
            tokio::select! {
                biased;
                _ = shutdown.changed() => {}
                _ = self.drain_commands() => return,
            }
        }
        self.batch_active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        for queue in self.queues.lock().await.iter_mut() {
            for cmd in queue.drain(..) {
                let _ = cmd.response_tx.send(Err(TransportError::Disconnected));
            }
        }
        if shutdown.has_changed().is_err() {
            let _ = self.transport.lock().await.close().await;
        }
    }

    async fn drain_commands(&self) {
        let mut active_batch = None;
        loop {
            let cmd = {
                let mut queues = self.queues.lock().await;
                let batch_priority = active_batch.and_then(|id| {
                    (0..4).find(|&p| {
                        queues[p]
                            .front()
                            .is_some_and(|cmd| cmd.batch_id == Some(id))
                    })
                });
                let priority =
                    batch_priority.or_else(|| (0..4).rev().find(|&p| !queues[p].is_empty()));
                priority.and_then(|p| queues[p].pop_front())
            };
            let Some(cmd) = cmd else {
                self.batch_active
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                return;
            };
            active_batch = cmd.batch_id;
            self.batch_active
                .store(active_batch.is_some(), std::sync::atomic::Ordering::Relaxed);
            let result = loop {
                if self
                    .needs_reconnect
                    .load(std::sync::atomic::Ordering::Relaxed)
                    && let Err(err) = self.reconnect_transport().await
                {
                    break Err(err);
                }
                let result = self.execute_queued_command(&cmd).await;
                if result.is_err() {
                    // An ambiguous exchange must be resynchronized before any send.
                    self.needs_reconnect
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                if cmd.replay_safe
                    && matches!(
                        result,
                        Err(TransportError::Disconnected
                            | TransportError::Hid(_)
                            | TransportError::Io(_))
                    )
                {
                    continue;
                }
                break result;
            };
            let _ = cmd.response_tx.send(result);
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
        self.needs_reconnect
            .store(true, std::sync::atomic::Ordering::Relaxed);
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

            let reopened = self.transport.lock().await.reconnect().await;
            match reopened {
                Ok(new_transport) => {
                    let mut transport = self.transport.lock().await;
                    *transport = new_transport;
                    self.needs_reconnect
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    self.disconnect_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    self.reconnect_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::info!("transport reconnected via {}", self.transport_url);
                    return Ok(());
                }
                Err(e) => {
                    self.disconnect_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::warn!("transport reconnect attempt {} failed: {}", attempt + 1, e);
                    attempt += 1;
                }
            }
        }
    }
}

// Only known reads and absolute-value writes can survive uncertain delivery.
// FIR staging, reset/persistence operations, and unknown opcodes stay excluded.
fn is_replay_safe(payload: &[u8]) -> bool {
    matches!(
        payload.first().copied(),
        Some(
            commands::CMD_READ_HARDWARE_ID
                | commands::CMD_READ_DSP_VERSION
                | commands::CMD_READ_FLOATS
                | commands::CMD_READ_FLASH
                | commands::CMD_READ_FLASH_FULL_ADDR
                | commands::CMD_READ_DFLASH_ID
                | commands::CMD_WRITE_DSP
                | commands::CMD_WRITE_BIQUAD
                | commands::CMD_BYPASS_FILTER
                | commands::CMD_SET_VOLUME
                | commands::CMD_SET_MUTE
                | commands::CMD_SET_SOURCE
                | commands::CMD_SET_CONFIG
                | commands::CMD_DIRAC_BYPASS
                | commands::CMD_SET_DISPLAY_BRIGHTNESS
                | commands::CMD_SET_DISPLAY_IDLE_TIME
                | commands::CMD_SET_DRE
                | commands::CMD_WRITE_FLASH
                | commands::CMD_WRITE_FLASH_FULL_ADDR
        )
    ) || matches!(payload, [commands::CMD_DIRAC_DELAY_GAIN, 0xA1, _])
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

pub(crate) async fn open_transport(url: &str) -> Result<Box<dyn Transport>, TransportError> {
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
        // Paths can be reused after hotplug; verify identity even at the old path.
        let devices = crate::transport::hid::enumerate()?;
        let device = devices
            .iter()
            .find(|device| usb.matches(device))
            .ok_or(TransportError::NotFound)?;
        let transport = HidTransport::open(&device.path)?;
        let opened = parse_usb_url(transport.url().strip_prefix("usb:").unwrap())?;
        if opened.serial != usb.serial || opened.vid != usb.vid || opened.pid != usb.pid {
            return Err(TransportError::NotFound);
        }
        return Ok(Box::new(transport));
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
    #[allow(dead_code)]
    path: String,
    vid: u16,
    pid: u16,
    serial: Option<String>,
}

impl ParsedUsbUrl {
    fn matches(&self, device: &crate::transport::hid::HidDeviceInfo) -> bool {
        self.serial.is_some()
            && self.serial == device.serial_number
            && self.vid == device.vendor_id
            && self.pid == device.product_id
    }
}

fn parse_usb_url(rest: &str) -> Result<ParsedUsbUrl, TransportError> {
    let (encoded_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = urlencoding::decode(encoded_path)
        .map_err(|e| TransportError::Io(format!("invalid usb path encoding: {e}")))?;

    let mut vid = None;
    let mut pid = None;
    let mut serial = None;
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
            "serial" if !value.is_empty() => {
                serial = Some(
                    urlencoding::decode(value)
                        .map_err(|e| {
                            TransportError::Io(format!("invalid usb serial encoding: {e}"))
                        })?
                        .into_owned(),
                );
            }
            _ => {}
        }
    }

    Ok(ParsedUsbUrl {
        path: path.into_owned(),
        serial,
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

    type ResponseQueue = Arc<StdMutex<VecDeque<Result<Vec<u8>, TransportError>>>>;

    #[derive(Clone)]
    struct FakeTransport {
        sent: Arc<StdMutex<Vec<Vec<u8>>>>,
        responses: ResponseQueue,
        recv_calls: Arc<AtomicUsize>,
        replacement: Arc<StdMutex<Option<Box<dyn Transport>>>>,
    }

    impl FakeTransport {
        fn new(responses: impl IntoIterator<Item = Result<Vec<u8>, TransportError>>) -> Self {
            Self {
                sent: Arc::new(StdMutex::new(Vec::new())),
                responses: Arc::new(StdMutex::new(responses.into_iter().collect())),
                recv_calls: Arc::new(AtomicUsize::new(0)),
                replacement: Arc::new(StdMutex::new(None)),
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
                tokio::task::yield_now().await;
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

        fn reconnect(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Box<dyn Transport>, TransportError>>
                    + Send
                    + '_,
            >,
        > {
            let replacement = self.replacement.lock().unwrap().take();
            Box::pin(async move {
                match replacement {
                    Some(transport) => Ok(transport),
                    None => open_transport(&self.url()).await,
                }
            })
        }

        fn url(&self) -> String {
            "fake://queue-test".to_string()
        }
    }

    struct UnavailableTransport {
        reconnect_calls: Arc<AtomicUsize>,
        close_calls: Arc<AtomicUsize>,
        block_reconnect: bool,
    }

    impl Transport for UnavailableTransport {
        fn send_frame(
            &self,
            _: &[u8],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>,
        > {
            Box::pin(async { Err(TransportError::Timeout) })
        }

        fn recv_frame(
            &self,
            _: u64,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>,
        > {
            Box::pin(async { Err(TransportError::Disconnected) })
        }

        fn is_connected(&self) -> bool {
            false
        }

        fn close(
            &self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>,
        > {
            self.close_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }

        fn reconnect(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Box<dyn Transport>, TransportError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                self.reconnect_calls.fetch_add(1, Ordering::SeqCst);
                if self.block_reconnect {
                    std::future::pending::<()>().await;
                }
                Err(TransportError::NotFound)
            })
        }

        fn url(&self) -> String {
            "mock://unavailable".into()
        }
    }

    fn unavailable_queue(
        block_reconnect: bool,
        max_retries: u32,
    ) -> (CommandQueue, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let reconnect_calls = Arc::new(AtomicUsize::new(0));
        let close_calls = Arc::new(AtomicUsize::new(0));
        let queue = CommandQueue::with_reconnect(
            Box::new(UnavailableTransport {
                reconnect_calls: reconnect_calls.clone(),
                close_calls: close_calls.clone(),
                block_reconnect,
            }),
            ReconnectConfig {
                base_delay: Duration::from_millis(1),
                max_delay: Duration::from_millis(5),
                max_retries,
                ..ReconnectConfig::default()
            },
        );
        (queue, reconnect_calls, close_calls)
    }

    async fn wait_for_reconnects(calls: &AtomicUsize, count: usize) {
        tokio::time::timeout(Duration::from_millis(200), async {
            while calls.load(Ordering::SeqCst) < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn close_stops_recovery_and_prevents_reopen() {
        for block_reconnect in [false, true] {
            let (queue, reconnect_calls, _) = unavailable_queue(block_reconnect, 0);
            assert!(
                queue
                    .execute(commands::fir_load_start(0), Priority::Critical)
                    .await
                    .is_err()
            );
            let worker = queue.clone();
            let request = tokio::spawn(async move {
                worker
                    .execute(commands::set_source(1), Priority::High)
                    .await
            });
            wait_for_reconnects(&reconnect_calls, 1).await;
            tokio::time::timeout(Duration::from_millis(200), queue.close())
                .await
                .unwrap()
                .unwrap();
            assert!(request.await.unwrap().is_err());
            let calls = reconnect_calls.load(Ordering::SeqCst);
            assert!(matches!(
                queue.execute(commands::set_source(2), Priority::High).await,
                Err(TransportError::Disconnected)
            ));
            assert!(
                queue
                    .execute_batch(vec![commands::set_source(3)], Priority::High)
                    .await
                    .is_err()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(reconnect_calls.load(Ordering::SeqCst), calls);
            assert!(queue.inner.needs_reconnect.load(Ordering::Relaxed));
        }
    }

    #[tokio::test]
    async fn last_handle_drop_stops_recovery_and_releases_transport() {
        for block_reconnect in [false, true] {
            let (queue, reconnect_calls, close_calls) = unavailable_queue(block_reconnect, 0);
            let last_handle = queue.clone();
            let inner = Arc::downgrade(&queue.inner);
            assert!(
                queue
                    .execute(commands::fir_load_start(0), Priority::Critical)
                    .await
                    .is_err()
            );
            let worker = queue.clone();
            let request = tokio::spawn(async move {
                worker
                    .execute(commands::set_source(1), Priority::High)
                    .await
            });
            wait_for_reconnects(&reconnect_calls, 1).await;
            request.abort();
            let _ = request.await;
            drop(queue);
            if !block_reconnect {
                wait_for_reconnects(&reconnect_calls, 2).await;
            }
            assert!(inner.upgrade().is_some());
            drop(last_handle);
            tokio::time::timeout(Duration::from_millis(200), async {
                while inner.upgrade().is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("executor must release its transport after the last handle drops");
            assert_eq!(close_calls.load(Ordering::SeqCst), 1);
            let calls = reconnect_calls.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(reconnect_calls.load(Ordering::SeqCst), calls);
        }
    }

    #[tokio::test]
    async fn failed_recovery_remains_required_before_next_send() {
        let (queue, reconnect_calls, _) = unavailable_queue(false, 1);
        assert!(
            queue
                .execute(commands::fir_load_start(0), Priority::Critical)
                .await
                .is_err()
        );
        for count in 1..=2 {
            assert!(
                queue
                    .execute(commands::set_source(1), Priority::High)
                    .await
                    .is_err()
            );
            assert!(queue.inner.needs_reconnect.load(Ordering::Relaxed));
            assert_eq!(reconnect_calls.load(Ordering::SeqCst), count);
            assert!(queue.disconnect_epoch() > 0);
        }
    }

    #[tokio::test]
    async fn disconnected_reads_and_absolute_writes_reconnect_and_replay() {
        let mut float_response = vec![commands::CMD_READ_FLOATS, 0, 0x44];
        float_response.extend_from_slice(&2.0f32.to_le_bytes());
        let cases = [
            (
                commands::read_hardware_id(),
                vec![commands::CMD_READ_HARDWARE_ID, 1, 0, 10],
            ),
            (
                commands::read_floats(0x44, 1, &Dialect::NEW),
                float_response,
            ),
            (
                commands::read_flash(0xff, 0xd8, 4),
                vec![commands::CMD_READ_FLASH, 0, 0, 0, 0],
            ),
            (commands::set_volume(20), vec![commands::RESP_OK]),
            (commands::set_mute(true), vec![commands::RESP_OK]),
            (commands::set_source(2), vec![commands::RESP_OK]),
            (
                commands::set_config(1, commands::PRESET_CHANGE_NORMAL),
                vec![commands::RESP_PRESET_COMPLETE],
            ),
            (
                commands::write_dsp(0x44, 1.0, true, &Dialect::NEW),
                vec![commands::RESP_OK],
            ),
            (
                commands::write_biquad(0x44, &[1.0; 5], &Dialect::NEW),
                vec![commands::RESP_OK],
            ),
        ];
        for (payload, response) in cases {
            let transport = FakeTransport::new([Err(TransportError::Disconnected)]);
            let sent = transport.sent.clone();
            let replacement = FakeTransport::new([Ok(response)]);
            let replacement_sent = replacement.sent.clone();
            *transport.replacement.lock().unwrap() = Some(Box::new(replacement));
            let queue = CommandQueue::with_reconnect(
                Box::new(transport),
                ReconnectConfig {
                    base_delay: Duration::ZERO,
                    ..ReconnectConfig::default()
                },
            );
            queue
                .execute(payload.clone(), Priority::Normal)
                .await
                .unwrap();
            assert_eq!(*sent.lock().unwrap(), vec![payload.clone()]);
            assert_eq!(*replacement_sent.lock().unwrap(), vec![payload]);
            assert_eq!(queue.disconnect_epoch(), 1);
        }
    }

    #[tokio::test]
    async fn typed_raw_word_read_reconnects_and_replays() {
        let payload = commands::read_floats(0x44, 1, &Dialect::NEW);
        let response = vec![commands::CMD_READ_FLOATS, 0, 0x44, 1, 0, 0, 0];
        let transport = FakeTransport::new([Err(TransportError::Io("bridge dropped".into()))]);
        *transport.replacement.lock().unwrap() =
            Some(Box::new(FakeTransport::new([Ok(response.clone())])));
        let queue = CommandQueue::new(Box::new(transport));
        assert_eq!(
            queue
                .execute_typed_raw_with_timeout(payload, Priority::Normal, Duration::from_secs(1))
                .await
                .unwrap(),
            response
        );
    }

    #[tokio::test]
    async fn stateful_unknown_and_bridge_commands_are_never_replayed() {
        for (payload, raw) in [
            (commands::fir_load_start(0), false),
            (commands::fir_load_data(0, &[1.0]), false),
            (commands::fir_load_end(), false),
            (commands::reset(), false),
            (commands::enter_bootloader(), false),
            (commands::copy_preset(true), false),
            (commands::write_flash_block(0, &[1], false), false),
            (commands::finish_flash_block(0), false),
            (vec![commands::CMD_ERASE_FLASH], false),
            (vec![0xfe, 1], false),
            (commands::set_source(1), true),
            (commands::read_hardware_id(), true),
        ] {
            let transport = FakeTransport::new([Err(TransportError::Disconnected)]);
            let replacement = FakeTransport::new([Ok(vec![commands::RESP_OK])]);
            let replacement_sent = replacement.sent.clone();
            *transport.replacement.lock().unwrap() = Some(Box::new(replacement));
            let queue = CommandQueue::new(Box::new(transport));
            if raw {
                assert!(matches!(
                    queue.execute_raw(payload, Priority::Normal).await,
                    Err(TransportError::Disconnected)
                ));
            } else {
                assert!(matches!(
                    queue.execute(payload, Priority::Normal).await,
                    Err(TransportError::Disconnected)
                ));
            }
            queue
                .execute(commands::set_source(2), Priority::High)
                .await
                .unwrap();
            assert_eq!(
                *replacement_sent.lock().unwrap(),
                vec![commands::set_source(2)]
            );
        }
    }

    #[tokio::test]
    async fn disconnected_fir_data_is_not_replayed_by_queue() {
        let transport = FakeTransport::new([Err(TransportError::Disconnected)]);
        let sent = transport.sent.clone();
        let replacement = FakeTransport::new([Ok(vec![commands::RESP_OK])]);
        let replacement_sent = replacement.sent.clone();
        *transport.replacement.lock().unwrap() = Some(Box::new(replacement));
        let queue = CommandQueue::new(Box::new(transport));
        assert!(matches!(
            queue
                .execute(commands::fir_load_data(0, &[1.0]), Priority::Critical)
                .await,
            Err(TransportError::Disconnected)
        ));
        queue
            .execute(commands::set_source(2), Priority::High)
            .await
            .unwrap();
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(
            *replacement_sent.lock().unwrap(),
            vec![commands::set_source(2)]
        );
    }

    #[tokio::test]
    async fn timeout_reconnect_discards_same_address_stale_read() {
        let response = |value: f32| {
            let mut raw = vec![commands::CMD_READ_FLOATS, 0, 0x44];
            raw.extend_from_slice(&value.to_le_bytes());
            raw
        };
        let transport = FakeTransport::new([Ok(response(1.0))]);
        let old_reads = transport.recv_calls.clone();
        *transport.replacement.lock().unwrap() =
            Some(Box::new(FakeTransport::new([Ok(response(2.0))])));
        let queue = CommandQueue::new(Box::new(transport));
        let request = commands::read_floats(0x44, 1, &Dialect::NEW);
        assert!(matches!(
            queue
                .execute_with_timeout(request.clone(), Priority::Normal, Duration::ZERO)
                .await,
            Err(TransportError::Timeout)
        ));
        assert_eq!(
            queue.execute(request, Priority::Normal).await.unwrap(),
            Response::Floats(vec![2.0])
        );
        assert_eq!(old_reads.load(Ordering::Relaxed), 0);
        assert_eq!(queue.reconnect_epoch(), 1);
    }

    #[test]
    fn usb_reconnect_requires_matching_serial_even_when_path_matches() {
        let usb = parse_usb_url("old?vid=2752&pid=0011&serial=unit%20A").unwrap();
        let mut device = crate::transport::hid::HidDeviceInfo {
            path: "old".into(),
            vendor_id: 0x2752,
            product_id: 0x0011,
            serial_number: Some("unit B".into()),
            product_string: None,
        };
        assert!(!usb.matches(&device));
        device.path = "new".into();
        device.serial_number = Some("unit A".into());
        assert!(usb.matches(&device));
        let unidentified = parse_usb_url("old?vid=2752&pid=0011").unwrap();
        assert!(!unidentified.matches(&device));
    }

    #[tokio::test]
    async fn level_pauses_nest_and_resume_when_a_paused_caller_is_cancelled() {
        let queue = CommandQueue::new(Box::new(FakeTransport::new([])));
        let outer = queue.pause_levels();
        let inner = queue.pause_levels();
        drop(inner);
        assert!(queue.is_levels_paused(), "outer pause still held");
        drop(outer);
        assert!(!queue.is_levels_paused());

        // A caller cancelled mid-operation (e.g. a dropped HTTP request) must
        // not leave polling paused.
        let paused = queue.clone();
        let task = tokio::spawn(async move {
            let _levels = paused.pause_levels();
            std::future::pending::<()>().await;
        });
        while !queue.is_levels_paused() {
            tokio::task::yield_now().await;
        }
        task.abort();
        let _ = task.await;
        assert!(!queue.is_levels_paused());
    }

    #[tokio::test]
    async fn cancelled_batch_does_not_strand_next_caller() {
        let transport = FakeTransport::new([]);
        let responses = transport.responses.clone();
        let recv_calls = transport.recv_calls.clone();
        let queue = Arc::new(CommandQueue::new(Box::new(transport)));
        let worker = queue.clone();
        let batch = tokio::spawn(async move {
            worker
                .execute_batch(
                    vec![commands::set_source(1), commands::set_source(2)],
                    Priority::High,
                )
                .await
        });
        while recv_calls.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
        batch.abort();
        let _ = batch.await;
        responses
            .lock()
            .unwrap()
            .extend((0..3).map(|_| Ok(vec![commands::RESP_OK])));
        let response = tokio::time::timeout(
            Duration::from_millis(200),
            queue.execute(commands::set_source(3), Priority::High),
        )
        .await;
        assert!(matches!(response, Ok(Ok(Response::Ack))));
        assert!(!queue.is_levels_paused());
    }

    #[tokio::test]
    async fn timeout_requires_recovery_before_another_send() {
        let transport = FakeTransport::new([Ok(vec![commands::RESP_OK])]);
        let sent = transport.sent.clone();
        let queue = CommandQueue::with_reconnect(
            Box::new(transport),
            ReconnectConfig {
                base_delay: Duration::ZERO,
                max_retries: 1,
                ..ReconnectConfig::default()
            },
        );
        assert!(matches!(
            queue
                .execute_with_timeout(commands::set_source(1), Priority::High, Duration::ZERO)
                .await,
            Err(TransportError::Timeout)
        ));
        assert!(
            queue
                .execute(commands::set_source(2), Priority::High)
                .await
                .is_err()
        );
        assert_eq!(sent.lock().unwrap().len(), 1);
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
            let mut queues = queue.inner.queues.lock().await;
            queues[Priority::High as usize].push_back(QueuedCommand {
                payload: commands::set_source(1),
                priority: Priority::High,
                response_tx: tx1,
                allow_unknown_response: false,
                replay_safe: true,
                batch_id: None,
                timeout: Duration::from_millis(5),
            });
            queues[Priority::High as usize].push_back(QueuedCommand {
                payload: commands::set_source(2),
                priority: Priority::High,
                response_tx: tx2,
                allow_unknown_response: false,
                replay_safe: true,
                batch_id: None,
                timeout: Duration::from_millis(5),
            });
        }

        std::thread::sleep(Duration::from_millis(20));

        queue.inner.process_next().await;
        queue.inner.process_next().await;

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

    #[test]
    fn write_flash_block_uses_extended_default_timeout() {
        assert_eq!(
            default_timeout_for_payload(&commands::write_flash_block(0x01, &[0x04, 0x88], true)),
            DEFAULT_SLOW_COMMAND_TIMEOUT
        );
    }

    #[test]
    fn finish_flash_block_uses_extended_default_timeout() {
        assert_eq!(
            default_timeout_for_payload(&commands::finish_flash_block(0x01)),
            DEFAULT_SLOW_COMMAND_TIMEOUT
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
        oversized_response.extend(std::iter::repeat_n(0xed, 55));
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

    /// Regression test: WriteFlashBlock (0x06) from the Device Console
    /// preset-save pipeline also comes back as an oversized echoed HID buffer
    /// on real hardware. The TCP bridge must trim that to the request length
    /// instead of leaking the trailing junk into the Device Console parser.
    #[tokio::test]
    async fn write_flash_block_response_trimmed_to_request_length() {
        let request = vec![0x06u8, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00];
        let mut oversized_response = request.clone();
        oversized_response.extend(std::iter::repeat_n(0xed, 54));
        assert_eq!(oversized_response.len(), 63);

        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(oversized_response)])));

        let resp = queue
            .execute_raw(request.clone(), Priority::High)
            .await
            .expect("execute_raw should succeed with oversized write flash block echo");

        assert_eq!(
            resp.len(),
            request.len(),
            "raw flash-block response should be trimmed to request length ({}), got {} bytes: {:02x?}",
            request.len(),
            resp.len(),
            resp
        );
        assert_eq!(
            &resp[..],
            &request[..],
            "trimmed flash-block response should equal the echoed command"
        );
    }

    /// Regression test: FinishFlashBlock (0x07) uses the same unframed HID
    /// echo behavior as WriteFlashBlock (0x06), so the queue must trim the
    /// fixed-size HID tail before returning the raw bytes to TCP clients.
    #[tokio::test]
    async fn finish_flash_block_response_trimmed_to_request_length() {
        let request = vec![0x07u8, 0x02];
        let mut oversized_response = request.clone();
        oversized_response.extend(std::iter::repeat_n(0xed, 61));
        assert_eq!(oversized_response.len(), 63);

        let queue = CommandQueue::new(Box::new(FakeTransport::new([Ok(oversized_response)])));

        let resp = queue
            .execute_raw(request.clone(), Priority::High)
            .await
            .expect("execute_raw should succeed with oversized finish flash block echo");

        assert_eq!(
            resp.len(),
            request.len(),
            "raw finish-flash response should be trimmed to request length ({}), got {} bytes: {:02x?}",
            request.len(),
            resp.len(),
            resp
        );
        assert_eq!(
            &resp[..],
            &request[..],
            "trimmed finish-flash response should equal the echoed command"
        );
    }

    /// Replay the Device Console preset-save / reload sequence from the
    /// successful direct-USB app log:
    ///
    /// 1. `0x06` WriteFlashBlock echoed as an oversized HID buffer
    /// 2. `0x07` FinishFlashBlock echoed as an oversized HID buffer
    /// 3. `0x05` ReadFlash verify-program
    /// 4. `0x04` WriteFlash modification token
    /// 5. `0x25` ChangePreset / reload-current-slot completion
    ///
    /// This is the strongest non-hardware proof we can get for the shared TCP
    /// bridge path: the exact command family that previously crashed the
    /// official Device Console now decodes, trims, and advances through the
    /// same sequence the direct-USB path already completes successfully.
    #[tokio::test]
    async fn device_console_preset_pipeline_replay_matches_expected_responses() {
        let write_flash_block = vec![0x06u8, 0x01, 0x04, 0x88, 0x97, 0x13, 0x0f, 0x00, 0x00];
        let mut write_flash_block_resp = write_flash_block.clone();
        write_flash_block_resp.extend(std::iter::repeat_n(0xed, 54));

        let finish_flash_block = vec![0x07u8, 0x02];
        let mut finish_flash_block_resp = finish_flash_block.clone();
        finish_flash_block_resp.extend(std::iter::repeat_n(0xed, 61));

        let verify_program_req = commands::read_flash(0xff, 0xa3, 1);
        let verify_program_resp = vec![commands::CMD_READ_FLASH, 0xff, 0xa3, 0x03];

        let save_token_req = commands::write_flash(0xff, 0xc8, &[0xb5, 0x80, 0xeb, 0x8e]);
        let save_token_resp = save_token_req.clone();

        let reload_preset_req = commands::set_config(0, commands::PRESET_CHANGE_RELOAD_SAME_SLOT);
        let reload_preset_resp = vec![commands::RESP_PRESET_COMPLETE];

        let queue = CommandQueue::new(Box::new(FakeTransport::new([
            Ok(write_flash_block_resp),
            Ok(finish_flash_block_resp),
            Ok(verify_program_resp),
            Ok(save_token_resp),
            Ok(reload_preset_resp),
        ])));

        let raw_write_flash_block = queue
            .execute_raw(write_flash_block.clone(), Priority::High)
            .await
            .expect("write flash block should trim to the echoed command");
        assert_eq!(raw_write_flash_block, write_flash_block);

        let raw_finish_flash_block = queue
            .execute_raw(finish_flash_block.clone(), Priority::High)
            .await
            .expect("finish flash block should trim to the echoed command");
        assert_eq!(raw_finish_flash_block, finish_flash_block);

        let verify_program = queue
            .execute(verify_program_req, Priority::Normal)
            .await
            .expect("verify-program read should parse after flash block writes");
        assert_eq!(verify_program, Response::FlashData(vec![0x03]));

        let save_token = queue
            .execute(save_token_req, Priority::High)
            .await
            .expect("save modification token should accept opcode echo ack");
        assert_eq!(save_token, Response::Ack);

        let reload_preset = queue
            .execute(reload_preset_req, Priority::Critical)
            .await
            .expect("reload-current-slot set_config should accept preset complete");
        assert_eq!(reload_preset, Response::PresetComplete);
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
