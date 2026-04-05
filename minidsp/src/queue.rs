//! Command queue with priority, batch execution, and level polling pause.

use crate::transport::{Transport, TransportError};
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

/// Per-device command queue with priority scheduling.
pub struct CommandQueue {
    transport: Box<dyn Transport>,
    queues: Mutex<[VecDeque<QueuedCommand>; 4]>,
    #[allow(dead_code)]
    state: Mutex<QueueState>,
    notify: Notify,
    levels_paused: std::sync::atomic::AtomicBool,
    next_batch_id: std::sync::atomic::AtomicU64,
    // We default to new dialect for parsing responses; the client overrides this
    dialect: Mutex<Dialect>,
}

impl CommandQueue {
    pub fn new(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
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
        }
    }

    pub async fn set_dialect(&self, dialect: Dialect) {
        *self.dialect.lock().await = dialect;
    }

    /// Execute a single command and return the parsed response.
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
        rx.await.map_err(|_| TransportError::Io("response channel closed".into()))?
    }

    /// Submit a batch of commands that execute contiguously.
    pub async fn execute_batch(
        &self,
        commands: Vec<Vec<u8>>,
        priority: Priority,
    ) -> Result<Vec<Response>, TransportError> {
        let batch_id = self.next_batch_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        self.levels_paused.store(true, std::sync::atomic::Ordering::Relaxed);
        self.notify.notify_one();

        // Process all queued commands
        for _ in 0..receivers.len() {
            self.process_next().await;
        }

        self.levels_paused.store(false, std::sync::atomic::Ordering::Relaxed);

        let mut responses = Vec::with_capacity(receivers.len());
        for rx in receivers {
            let qr = rx.await
                .map_err(|_| TransportError::Io("response channel closed".into()))??;
            responses.push(qr.parsed);
        }
        Ok(responses)
    }

    /// Check if level polling is paused.
    pub fn is_levels_paused(&self) -> bool {
        self.levels_paused.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Process the next command in the queue.
    async fn process_next(&self) {
        let cmd = {
            let mut queues = self.queues.lock().await;
            // Pick highest-priority non-empty queue
            let cmd = (0..4)
                .rev()
                .find_map(|p| queues[p].pop_front());
            cmd
        };

        let Some(cmd) = cmd else { return };

        // Check deadline
        if Instant::now() > cmd.deadline {
            let _ = cmd.response_tx.send(Err(TransportError::Timeout));
            return;
        }

        // Send raw payload — transport handles wire encoding
        if let Err(e) = self.transport.send_frame(&cmd.payload).await {
            let _ = cmd.response_tx.send(Err(e));
            return;
        }

        // Receive decoded payload from transport
        match self.transport.recv_frame(500).await {
            Ok(raw) => {
                let dialect = self.dialect.lock().await;
                let response = commands::parse_response(&raw, &dialect);

                // Handle preset change: wait for PresetComplete
                if cmd.payload.first() == Some(&commands::CMD_SET_CONFIG) {
                    if matches!(response, Response::Ack) {
                        match self.transport.recv_frame(5000).await {
                            Ok(raw2) => {
                                let resp2 = commands::parse_response(&raw2, &dialect);
                                let _ = cmd.response_tx.send(Ok(QueueResponse { parsed: resp2, raw: raw2 }));
                                return;
                            }
                            Err(_) => {
                                let _ = cmd.response_tx.send(Ok(QueueResponse { parsed: response, raw: raw.clone() }));
                                return;
                            }
                        }
                    }
                }

                let _ = cmd.response_tx.send(Ok(QueueResponse { parsed: response, raw }));
            }
            Err(e) => {
                let _ = cmd.response_tx.send(Err(e));
            }
        }
    }
}
