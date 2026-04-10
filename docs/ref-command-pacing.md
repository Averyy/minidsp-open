# Command Pacing and Serialization Research

Research findings for minidsp-open command queue architecture.

## Concurrency Rules

All daemon-managed paths to the device go through a single-executor command queue. Standalone CLI runs can still talk to USB/TCP directly when no daemon is in the path.

| Interface | Connection limit | Priority | Arbitration |
|-----------|-----------------|----------|-------------|
| **TCP bridge (port 5333)** | 1 connection (WI-DG compat) | NORMAL | Second connection rejected. Commands decoded and queued. |
| **HTTP API (port 5380)** | Unlimited | HIGH for writes, NORMAL for reads | Queue depth limit (64) → 503 if full. |
| **WebSocket levels** | Unlimited (subscribe to broadcast) | LOW | One shared poll task, droppable. Paused during batch ops. |
| **CLI via daemon** | N/A (talks to HTTP API) | Same as HTTP | Uses the daemon queue when pointed at the daemon. |
| **Device Console** | Via TCP bridge (counts as the 1 TCP client) | NORMAL | Coexists with HTTP clients through the queue. |

## 1. USB HID Round-Trip Time

### Theoretical Limits

MiniDSP devices are USB Full Speed (12 Mbps) HID devices. Key constraints:

- **Endpoint type:** Interrupt IN and OUT
- **Report size:** 65 bytes (1 byte report ID + 64 bytes data)
- **USB Full Speed HID polling:** bInterval typically 1-10ms. The USB spec (5.7.4) says the host polls at least as often as bInterval, rounded down to a power of two by most host controllers (e.g., bInterval=9 becomes 8ms actual)
- **Theoretical minimum per command:** ~2ms (1ms OUT poll + 1ms IN poll if bInterval=1)
- **Practical minimum:** ~3-5ms per round-trip for simple commands (write + read ACK)

### Measured Data From minidsp-rs

Issue #383 provides the most precise timing data available. From the maintainer's debug log (MiniDSP Flex, preset change):

```
[0.000s] Sent: SetConfig
[0.003s] Recv: ConfigChanged (ACK)    <- 3ms round-trip for simple ACK
[0.003s] Sent: ReadMemory
[0.006s] Recv: MemoryData             <- 3ms round-trip for a memory read
[0.507s] Sent: ReadMemory (retry)
[4.672s] Recv: MemoryData (new value) <- 4.6 second device-side processing delay for preset switch
```

This confirms:
- **Simple command round-trip: ~3ms** (command + ACK or short response)
- **ReadFloats/ReadMemory round-trip: ~3ms** (same as simple commands)
- **Preset switch: 4-5 seconds** device-side processing time (Flex-specific; audio codec reconfiguration)

### Throughput Ceiling

At ~3ms per round-trip, theoretical max throughput is ~333 commands/second. With real-world jitter, USB scheduling, and OS overhead:
- **Practical sustained throughput: ~150-250 commands/second**
- **Conservative safe rate: ~100 commands/second** (10ms inter-command)

### HID Write Retry Behavior (minidsp-rs)

The `stream.rs` HID write implementation reveals important behavior:
```rust
// In HidStream::send()
let mut remaining_tries = 10;
let mut result = device.write(&buf);
while let Err(e) = result {
    thread::sleep(time::Duration::from_millis(250));
    result = device.write(&buf);
    remaining_tries -= 1;
    if remaining_tries == 0 { return result; }
}
```

minidsp-rs retries USB writes up to 10 times with 250ms delays between attempts. This means a single failed write can block the transport for up to 2.5 seconds. The read side uses a 500ms timeout per poll cycle.

## 2. What Happens When Commands Are Sent Too Fast

### Protocol Error Response (0xFF)

The protocol doc lists 0xFF as "buffer overflow / fatal." From Device Console decompilation, the `deviceCmdQueue.js` implements a strict single-command-in-flight model: one command is dispatched, the queue waits for the response, then dispatches the next. This is the safest approach and matches the protocol's synchronous nature.

### Device Input Queue

Evidence from the minidsp-rs multiplexer code indicates the device does NOT have a meaningful input queue:
- The `Multiplexer` maintains a `VecDeque<PendingCommandTuple>` and sends commands sequentially
- Command-response matching is done by checking `cmd.matches_response(&data)` because the device has NO command tagging -- responses lack correlation IDs
- The protocol is inherently synchronous: send one command, get one response

If a second command arrives while the device is processing the first, the behavior is undefined -- it may be dropped, may corrupt the first response, or may trigger the 0xFF overflow. The Device Console's strict one-at-a-time model confirms this is a real risk.

### Observed Failure Modes

From minidsp-rs issues and community reports:

1. **Issue #570 (level streaming blocks preset changes):** When a WebSocket client has `levels=true`, the 250ms level polling consumes the command channel. POST requests to change presets silently fail -- no error returned to the HTTP client because the request appears to succeed at the HTTP layer but the device never processes it (the level read consumed the command slot).

2. **Issue #699 (simultaneous connections corrupt state):** When multiple clients (Device Console + minidsp-rs CLI) issue commands through the same TCP server, the interleaved commands cause Device Console JavaScript assertion errors. The Device Console expects a specific response to each command it sends, but receives responses to commands injected by other clients.

3. **Issue #383 (Flex input switching delay):** After sending SetConfig (preset change), the device immediately sends a config-change acknowledgement and may still take 4-5 seconds to actually apply the change. Commands sent during this window may target the wrong preset.

Live 2x4 HD note from this repo: on the physical DSP100 / 2x4 HD path tested on 2026-04-09, raw `SetConfig` returns an immediate `Ack` and the preset readback flips almost immediately. The shared queue should therefore return the first valid preset response right away, while the high-level client verifies completion by polling master status instead of waiting for a second transport-level preset-only frame.

The official raw Device Console HID capture from 2026-04-09 also tightened two queue/parser rules for this device family:
- successful writes can come back as report-ID-prefixed same-opcode echoes, not just bare `0x01` acknowledgements
- `SetSource` can be followed by a stale `0x05 ff a9 {source}` flash notification that must not be consumed as the response to the next unrelated read
- the official preset-save path is a longer flash workflow, not an interactive runtime-state read:
  - `0x06` / `0x07` are `WriteFlashBlock` / `FinishFlashBlock` chunks for preset persistence
  - the 2x4 HD save flow then verifies with `0x05 ff a3 01`, writes the modification token at `0xffc8`, and ends with `0x25 slot 0x01`
- the companion official session log shows the same controller-side state model even for normal audio-proc edits:
  - Device Console compares the device token against its XML token and then explicitly saves a fresh active-preset token after successful preset-content updates
  - raw DSP writes on the physical 2x4 HD do not auto-advance `0xFFC8`, so controllers have to write the freshness marker themselves
  - the desktop daemon now mirrors that behavior for `/config` and measurement-mode preset mutations before persisting shadow state
- `CopyPreset (0x27)` is a genuinely long-running flash operation on this device family, not a fast-ack write:
  - official Device Console on the same physical 2x4 HD sent `0x27 0x00` at `2026-04-08 15:14:04.417` and only logged `Received OK` at `2026-04-08 15:14:27.284`
  - this repo now gives `0x27` a longer shared queue deadline, which was live-verified on 2026-04-09 by `POST /devices/0/preset/copy` completing in `22.92 s`
- `Reset (0x03)` is also slow enough on the physical 2x4 HD to need a longer shared deadline:
  - live desktop verification on 2026-04-09 showed `HID OUT payload: [03]` at `22:29:42.994` and `HID IN payload: [aa]` at `22:29:49.557`, i.e. about `6.56 s`
  - this repo now gives `0x03` the same longer shared queue deadline so raw reset through the daemon bridge can complete instead of timing out at `10 s`
- Live 2x4 HD websocket note from this repo: with `/devices/0?levels=true&poll=true` active on 2026-04-09, `POST /devices/0/preset/0` and `/preset/1` still succeeded, but each took about `2.83 s` instead of the `20-30 ms` seen without a live polling socket.
  - clean reruns on the same daemon/build showed `POST /devices/0/source/Usb` / `/source/Analog` still completing in about `11 ms` / `6 ms` with the polling socket attached, so the slow path is specifically preset switching rather than all writes
  - the official raw Device Console capture shows the same preset-change pattern on the physical 2x4 HD: `0x25` was acknowledged immediately at `2026-04-09T19:30:52.875Z`, but the first follow-up `0x14` status reply did not arrive until `2026-04-09T19:30:56.185Z`
  - this repo now matches the official websocket meter cadence (`500 ms`) and avoids the extra immediate post-connect poll, so the remaining preset latency on the tested path is device/apply-time behavior, not the old silent-failure queue bug
- Live 2x4 HD status note from this repo on 2026-04-10: low-priority DSP100 meter reads can still fail or return malformed data on the attached 2x4 HD, so the daemon now treats them as best-effort and clamps failed reads to `-140 dB` silence instead of letting `GET /devices/{id}` stall or fail.

4. **Issue #765 (unreachable after standby):** After the device enters standby, the USB connection drops. minidsp-rs has no reconnection logic, so the daemon becomes permanently stuck.

### The ezBEQ Timing Problem

From the ezBEQ source code (`minidsp.py`), the `__do_run` method reveals the `slotChangeDelay` configuration option. When a filter load requires a preset switch:

```python
if slot_change_delay:
    self.__do_run([], slot, False)  # Switch preset first, alone
    if slot_change_delay is not True and slot_change_delay > 0:
        sleep(slot_change_delay)    # Wait for device to settle
```

ezBEQ had to add this configurable delay because preset switches on some devices (especially the Flex) need seconds to complete. Without it, the PEQ writes that follow land on the wrong preset.

For a typical ezBEQ filter load (2x4HD, 10 PEQ filters on 2 input channels):
- 20 WriteBiquad commands (10 per channel)
- 20 WriteBiquadBypass commands
- Total: ~40 commands at ~3ms each = **~120ms** to load a full BEQ filter set
- Plus optional preset switch (4-5s for Flex, near-instant for 2x4HD)

## 3. How minidsp-rs Handles This Today

### Architecture Overview

minidsp-rs has three layers of transport abstraction:

1. **Hub** (`hub.rs`): Frame-level multiplexer. Broadcasts all received frames to all connected clients via `tokio::sync::broadcast`. All clients share a single write channel (`mpsc::channel(100)`). The Hub provides `try_clone()` for multiple consumers. This is used by the TCP bridge and WebSocket bridge.

2. **Multiplexer** (`multiplexer.rs`): Command-response correlator. Sits above the Hub. Maintains a `VecDeque` of pending `(Command, oneshot::Sender)` pairs. On each received response, checks if the front of the queue matches (`cmd.matches_response()`). If yes, delivers via oneshot. If no, pushes to the broadcast event channel.

3. **Client** (`client.rs`): High-level API. Wraps the Multiplexer as a `tower::Service`. Adds a 10-second timeout per command via `tower::timeout`.

### Critical Serialization Points

**The write path is serialized by a `tokio::sync::Mutex`:**

```rust
// In Multiplexer::roundtrip()
let mut writer = this.write.lock().await;
writer.send(cmd).await?;
```

This means concurrent `roundtrip()` calls will queue at the Mutex. However, the pending command is registered in the `VecDeque` BEFORE acquiring the write lock:

```rust
let rx = {
    let (tx, rx) = oneshot::channel();
    let mut pending_command = this.pending_command.lock().unwrap();
    pending_command.push_back((cmd.clone(), tx));
    rx
};
let mut writer = this.write.lock().await;
```

This creates a race condition: if two commands are submitted concurrently, both are pushed to `pending_command` before either acquires the write lock. The responses then rely on `matches_response()` to route correctly. For many command types this works because the response format differs (e.g., Ack vs MemoryData). But for commands with identical response types (two consecutive WriteDSP commands both expecting Ack), the first response will match the first queued command, which may or may not be the one actually sent first -- depending on Mutex acquisition order.

**The SharedService wraps the Multiplexer in another Mutex:**

```rust
pub type SharedService = Arc<Mutex<MultiplexerService>>;
```

The Client locks this for each roundtrip. So concurrent Client calls are actually serialized at this level too -- but only within the same Client. Multiple Clients sharing the same SharedService contend on this lock.

### Level Meter Interaction (the #570 bug)

The HTTP WebSocket handler for levels uses a `tokio::time::interval(250ms)` with a `try_lock` guard:

```rust
IntervalStream::new(tokio::time::interval(Duration::from_millis(250)))
    .filter_map(move |_| {
        let device = levels_device.clone();
        async move {
            let device = device.try_lock().ok()?;
            let (input_levels, output_levels) = device.get_input_output_levels().await.ok()?;
            // ...
        }
    })
```

The `try_lock()` prevents multiple level reads from stacking up, but there is NO coordination between level reads and other commands. A level read holds the `SharedService` Mutex while its `read_floats_multi` chain executes (potentially multiple ReadFloats commands), blocking any other command. This is exactly what causes #570: the level polling holds the lock, the preset change request waits at the Mutex, the level poll completes, the preset change goes through, the next level poll starts immediately (250ms later), and the cycle repeats -- but the device may still be processing the preset change.

### TCP Bridge (no serialization at all)

The TCP bridge (`tcp/mod.rs`) is raw frame forwarding:

```rust
async fn forward<T>(tcp: T, device: Transport) -> Result<()> {
    // ...
    loop {
        select! {
            frame = device.next() => { remote.send(frame?).await? },
            frame = remote.next() => { device.send(frame?).await? },
        }
    }
}
```

There is NO command-response correlation, NO serialization, NO busy detection. Each TCP client gets a Hub clone, and all clients' writes go to the same device. This is how #699 happens: Device Console sends a command via TCP, the CLI injects another command via the HTTP API (which goes through the Multiplexer on a separate Hub clone), and the responses get cross-wired.

### What minidsp-rs Gets Right

- Single-writer Mutex on the transport (prevents garbled frames)
- Command-response matching via `matches_response()` (works for most cases)
- Timeout (10 seconds per command, catches stuck devices)
- Write retries (10 attempts with 250ms backoff for USB errors)

### What minidsp-rs Gets Wrong

- No priority between commands (level reads block everything)
- No coordination between Hub clients (TCP bridge + HTTP API fight)
- No "busy" response to callers (commands silently queue or fail)
- No pause mechanism for level polling during batch operations
- Preset switch doesn't gate subsequent commands (sends immediately after ACK, before device is ready)
- No reconnection after USB disconnect or standby/wake

## 4. MiniDSP Device Console Approach

From the deobfuscated `deviceCmdQueue.js`:

The Device Console implements a strict **single-command-in-flight queue**:

1. `add(cmd)` pushes to `cmdQ` array, returns a Promise
2. If no command is currently awaiting a reply (`!waitingReply`), calls `dispatchNext()`
3. `dispatchNext()` shifts one command from queue, calls `sendDspCmd()`, sets `waitingReply = true`
4. `msgReceived()` is called when a response arrives, sets `waitingReply = false`, calls `dispatchNext()`
5. `disconnectPending` flag: rejects all new commands during disconnect
6. `ignoreExceptCmd`: allows only one specific command type (used during firmware operations)
7. `reset()`: clears the queue and resets waitingReply

The level meter (`levelMeters.js`) uses `setTimeout` with a configurable interval (`timerIntervalMs`). Each tick calls `readDspParam()` (which goes through the command queue), and only schedules the next tick in the `finally` block after the read completes. This means level meter reads naturally yield to other commands -- if a PEQ write is queued while the meter is between reads, it gets dispatched before the next meter read.

**This is the correct architecture.** Device Console never has more than one command in flight, and the level meter doesn't starve other operations.

## 5. How CamillaDSP Handles This

CamillaDSP is a fundamentally different architecture -- it's a software DSP, not a hardware controller. But its websocket server approach is instructive:

- **Per-client threads:** Each WebSocket client gets its own thread, reads commands synchronously
- **Shared state via Arc<Mutex>/Arc<RwLock>:** `processing_params`, `capture_status`, etc.
- **Config changes via crossbeam channel:** `command_sender.try_send(ControllerMessage::ConfigChanged(...))`
- **Rate limiting:** Returns `RateLimitExceededError` if the command channel is full (`TrySendError::Full`)
- **No command queue for status reads:** Level/status data is read from shared atomic state, not by querying hardware. This is possible because CamillaDSP IS the DSP -- it already has the values in memory

Key insight: CamillaDSP's level meters are zero-cost reads from shared memory. MiniDSP level meters require actual USB round-trips. This makes command pacing much more critical for MiniDSP.

## 6. Concrete Queue Architecture Recommendation

### Priority Levels

Four priority levels, higher number = higher priority:

| Priority | Category | Examples | Rationale |
|----------|----------|----------|-----------|
| 0 (LOW) | Telemetry reads | Level meters, compressor meters | Non-critical, can skip/drop |
| 1 (NORMAL) | Status reads | get_master_status, get_device_info | Informational, should complete but can wait |
| 2 (HIGH) | Config writes | PEQ, gain, delay, mute, volume, source | User-initiated, should not be starved |
| 3 (CRITICAL) | State transitions | Preset switch, FIR load, measurement mode enter/exit | Must complete atomically, blocks all else |

### Queue Design

```
                                   +------------------+
  HTTP handler ----+               |                  |
  WebSocket -------+---> Priority  |   Command        |---> USB HID / TCP
  CLI command -----+     Queue     |   Executor       |     (single writer)
  TCP bridge ------+               |   (single task)  |
  Timer (levels) --+               +------------------+
```

**Single executor task** owns the transport. No other code touches USB/TCP writes directly.

```rust
struct CommandQueue {
    /// Priority queue: BinaryHeap or 4 separate VecDeques
    queues: [VecDeque<QueuedCommand>; 4],
    /// Current state
    state: QueueState,
    /// Notify when new commands arrive
    notify: tokio::sync::Notify,
}

enum QueueState {
    Idle,
    AwaitingResponse { cmd: Commands, deadline: Instant },
    BatchInProgress { remaining: usize, priority: u8 },
    DeviceBusy { until: Instant }, // Post-preset-switch cooldown
}

struct QueuedCommand {
    command: Commands,
    priority: u8,
    response_tx: oneshot::Sender<Result<Responses>>,
    /// For batch operations: group ID
    batch_id: Option<u64>,
    /// Deadline for this command (default 10s)
    deadline: Instant,
}
```

### Executor Loop

```
loop {
    1. If state is DeviceBusy, sleep until cooldown expires
    2. Pick highest-priority non-empty queue
    3. If batch_id matches current batch, stay in that queue
    4. Send command to device
    5. Set state = AwaitingResponse with deadline
    6. Wait for response OR deadline
    7. On response: deliver via oneshot, set state = Idle
    8. On timeout: deliver error via oneshot, consider reconnect
    9. On preset switch response: set state = DeviceBusy(5s)
       then poll device status until preset confirms changed
}
```

### Batch Operations

ezBEQ filter loads should be atomic from the queue's perspective:

```rust
/// Submit a batch of commands that must execute contiguously
async fn submit_batch(
    &self,
    commands: Vec<(Commands, oneshot::Sender<Result<Responses>>)>,
    priority: u8,
) -> Result<()> {
    let batch_id = self.next_batch_id();
    for (cmd, tx) in commands {
        self.enqueue(QueuedCommand {
            command: cmd,
            priority,
            response_tx: tx,
            batch_id: Some(batch_id),
            deadline: Instant::now() + Duration::from_secs(30),
        });
    }
    Ok(())
}
```

When the executor starts a batch, it continues pulling from the same queue until all commands with that `batch_id` are dispatched. Other priorities are deferred (but if a CRITICAL command arrives, it can interrupt -- this is a design choice; recommend NOT interrupting batches, since partial PEQ writes leave the device in an inconsistent state).

### Level Meter Behavior

- Level polling is a dedicated task with configurable interval (default 250ms)
- After each read completes, the task checks a `pause_levels` flag
- When a batch operation starts, set `pause_levels = true`
- When the batch completes, set `pause_levels = false`
- Level poll uses a `try_send` with timeout -- if the queue rejects it (full), skip this cycle
- Level commands have priority 0 and are droppable (the oneshot sender can be dropped without error)

```rust
async fn level_poll_task(queue: Arc<CommandQueue>, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        if queue.is_levels_paused() {
            continue;
        }
        let (tx, rx) = oneshot::channel();
        match queue.try_enqueue(QueuedCommand {
            command: Commands::ReadFloats { addr, len },
            priority: 0,
            response_tx: tx,
            batch_id: None,
            deadline: Instant::now() + Duration::from_secs(2),
        }) {
            Ok(()) => {
                // Wait for response, broadcast to WebSocket clients
                if let Ok(Ok(resp)) = rx.await {
                    broadcast_levels(resp);
                }
            }
            Err(_) => {
                // Queue full or paused, skip this cycle
                continue;
            }
        }
    }
}
```

### Inter-Command Delay

- **No mandatory inter-command delay** for normal operation (the device handles back-to-back commands fine as long as each waits for its response)
- **Post-preset-switch delay:** Poll master status every 500ms until the new preset is confirmed (already done by minidsp-rs v0.1.7+). During this period, the queue is in `DeviceBusy` state
- **Post-FIR-load delay:** The device auto-mutes during FIR coefficient loading. Wait for FIR load end ACK, then restore mute state (minidsp-rs already does this)
  - Live 2x4 HD note from this repo on 2026-04-09: the `FirLoadEnd (0x3B)` `Ack` can arrive a few seconds after the packet is sent. The queue must keep polling until the overall command deadline; treating each 500 ms receive poll timeout as a fatal command timeout breaks real FIR loads.
- **No artificial delays between PEQ writes** -- the protocol handles it as long as we wait for each ACK

### Handling "Busy"

Three strategies depending on caller type:

| Caller | On busy | Implementation |
|--------|---------|----------------|
| Level meter | Skip this cycle | `try_enqueue` returns `Err`, loop continues |
| HTTP API (one-shot) | Queue and wait | `enqueue` + `rx.await`, up to 10s timeout |
| HTTP API (config write) | Queue as batch | Batch with HIGH priority, response after all complete |
| TCP bridge | Queue and forward | Priority NORMAL, transparent to Device Console |
| WebSocket status poll | Queue and wait | Priority NORMAL, oneshot with 5s timeout |

If the queue exceeds a depth limit (e.g., 64 commands), new NORMAL/LOW commands get rejected with a "device busy" error. HIGH and CRITICAL always enqueue.

### TCP Bridge Integration

The TCP bridge is the trickiest client. Device Console expects raw frame-level access, but we need to serialize through the queue. Solution:

```rust
async fn tcp_bridge_handler(frame: Bytes, queue: Arc<CommandQueue>) -> Result<Bytes> {
    let cmd = Commands::decode(frame)?;
    let (tx, rx) = oneshot::channel();
    queue.enqueue(QueuedCommand {
        command: cmd,
        priority: 1, // NORMAL -- Device Console commands are user-interactive
        response_tx: tx,
        batch_id: None,
        deadline: Instant::now() + Duration::from_secs(10),
    });
    let response = rx.await??;
    Ok(response.encode())
}
```

This means Device Console commands go through the same queue as everything else. The device console's own internal `DeviceCmdQueue` handles its own pacing, and our queue ensures no conflicts with other clients.

### Multi-Device Support

Each physical device gets its own `CommandQueue` and executor task. No global lock. Device lookup by serial number or index, same as minidsp-rs.

### ESP32 Adaptation

On ESP32 (embassy), replace:
- `tokio::sync::Mutex` with `embassy_sync::mutex::Mutex`
- `tokio::sync::Notify` with `embassy_sync::signal::Signal`
- `VecDeque` with `heapless::Deque` (fixed capacity, no alloc)
- `oneshot::channel` with `embassy_sync::channel::Channel` (capacity 1)
- Max queue depth: 16 commands (memory constrained)
- Level polling interval: 500ms (slower, preserve bandwidth for commands)

### Summary of Key Differences From minidsp-rs

| Aspect | minidsp-rs | minidsp-open |
|--------|-----------|--------------|
| Serialization | Mutex on transport write | Single executor task, all commands queued |
| Level meters | Uncoordinated 250ms timer | Priority 0, pauseable, droppable |
| Batch operations | Sequential awaits, no grouping | Atomic batches with contiguous execution |
| Multi-client | Hub broadcasts, no arbitration | All clients submit to same priority queue |
| TCP bridge | Raw frame forwarding | Decoded, queued, re-encoded |
| Preset switch | ACK-based with status polling | DeviceBusy state, gates all commands |
| Busy handling | Silent queueing at Mutex | Explicit: reject, queue, or skip by caller type |
| Reconnection | None | Auto-reconnect with exponential backoff |
| Error propagation | Some commands silently fail | All commands get a response or timeout error |
