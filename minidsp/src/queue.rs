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
    #[allow(dead_code)]
    batch_id: Option<u64>,
    deadline: Instant,
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
        let qr = self.execute_inner(payload, priority).await?;
        Ok(qr.parsed)
    }

    /// Execute a command and return raw response bytes (for TCP bridge passthrough).
    pub async fn execute_raw(
        &self,
        payload: Vec<u8>,
        priority: Priority,
    ) -> Result<Vec<u8>, TransportError> {
        let qr = self.execute_inner(payload, priority).await?;
        Ok(qr.raw)
    }

    async fn execute_inner(
        &self,
        payload: Vec<u8>,
        priority: Priority,
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
                batch_id: None,
                deadline: Instant::now() + Duration::from_secs(10),
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
                    batch_id: Some(batch_id),
                    deadline: Instant::now() + Duration::from_secs(30),
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

        // Check deadline
        if Instant::now() > cmd.deadline {
            let _ = cmd.response_tx.send(Err(TransportError::Timeout));
            return;
        }

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

        transport.send_frame(&cmd.payload).await?;

        let raw = transport.recv_frame(500).await?;
        let dialect = self.dialect.lock().await;
        let response = commands::parse_response_for_request(&cmd.payload, &raw, &dialect);

        // Handle preset change: wait for PresetComplete
        if cmd.payload.first() == Some(&commands::CMD_SET_CONFIG)
            && matches!(response, Response::Ack)
        {
            match transport.recv_frame(5000).await {
                Ok(raw2) => {
                    let resp2 = commands::parse_response_for_request(&cmd.payload, &raw2, &dialect);
                    return Ok(QueueResponse {
                        parsed: resp2,
                        raw: raw2,
                    });
                }
                Err(TransportError::Disconnected) => return Err(TransportError::Disconnected),
                Err(_) => {
                    return Ok(QueueResponse {
                        parsed: response,
                        raw,
                    });
                }
            }
        }

        Ok(QueueResponse {
            parsed: response,
            raw,
        })
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
