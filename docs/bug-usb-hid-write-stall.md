# Bug: the 2x4 HD stops answering HID over the ESP32

Status: open. Recovery is automatic; the cause is unknown.

## What happens

During a long burst of HID writes, usually a snapshot restore, one write from
the ESP32 to the MiniDSP never completes. From then on the MiniDSP answers
nothing over HID. Until 2026-10-30 the board also froze: its HTTP server stopped
answering until the stall supervisor restarted it 30 s later, and the restore in
flight failed.

Seen only on a 2x4 HD driven by the ESP32-S3. The desktop direct-USB path has
not been run against the same workload.

Frequency on 2026-10-30:

- 3 stalls in about 10 snapshot restores during suite runs (one right after a
  MiniDSP reboot, one right after a preset copy, one in a boot that had played no
  audio).
- 2 stalls in about 115 restores across dedicated loops: one right after a short
  `/play` stream, one in the suite right after `POST /preset/copy`.
- 0 in 35 back-to-back restores with nothing else running, 0 in 40 rounds of
  `/play` then restore, 0 in 15 rounds of preset copy then restore.

So it is rare and doesn't reproduce on demand. Every stall so far started at
the first write of a `write_biquad`, but most restore commands are biquad
writes, so that may mean nothing.

## Evidence

The ESP32 logs the USB OTG controller (Synopsys DWC2) registers when a write
times out and again 2 s into the halt that follows (`USB DWC …` lines; the same
registers are at `GET /debug/usb`). From the two dumps captured:

- The HID OUT channel (endpoint 0x02, interrupt) is enabled and scheduled in all
  32 frame-list slots. Its transfer descriptor is still active with all 64 bytes
  pending (`qtd=86000040`), and no channel interrupt is pending (`hcint=0`).
- The DMA address register (`hcdmab`) sits 0x40 past the descriptor's buffer,
  so the controller fetched the packet and tried to send it.
- Frames keep running (the frame number advances 3 per 3 ms), the port is
  enabled at full speed and not suspended, and the periodic TX FIFO and request
  queue are empty.
- In the second stall the board also polled the HID IN endpoint before giving
  up. That channel showed the same state: enabled, descriptor active, nothing
  received within 500 ms.

Read together: the controller keeps offering the transfers and the MiniDSP
NAKs them in both directions. The device side, or the bus state the device sees,
stops making progress.

The board-side freeze is a separate, understood problem. Cleanup halts the
stuck channel, and ESP-IDF's host library waits for the halt interrupt with no
timeout (`_pipe_cmd_halt`, `ulTaskNotifyTake(..., portMAX_DELAY)` in
`hcd_dwc.c`). For a periodic channel the device keeps NAKing in descriptor-DMA
mode, that interrupt never comes: with ChEna and ChDis both set, the channel
stayed enabled for as long as we watched. Linux's dwc2 driver records the same
missing ChHalted in descriptor DMA (`hcd_ddma.c`, "Channel halted but no
subsequent ChHalted interrupt") and releases the channel without waiting. It
also leaves descriptor DMA off for full-speed devices by default; ESP-IDF's host
stack only supports descriptor DMA.

## Ruled out

- **Leftover data in the periodic TX FIFO.** The FIFO holds 150 words; the
  dump's 150 free words are its idle state, measured at boot with no audio.
- **The MiniDSP holding a report it wants read first.** Polling IN at the
  timeout returned nothing; the device NAKed IN too.
- **Command rate.** A restore issues about 55 commands a second, well under the
  measured safe rate of about 100.
- **Audio streaming.** One stall happened in a boot that had played nothing.
- **Logging.** The per-write log lines cost about 3 s per restore, and the stall
  happens at WARN level too.

## What the firmware does now

1. A write that gets no completion in 5 s gets 20 s more, and the board logs
   `USB OUT accepted … after the 5s timeout` if it then completes. No stall has
   come back within that window yet.
2. If the write is still stuck, the board powers the root port off, cleans up
   (with the port invalid, the host library marks the channels halted without
   waiting), and powers it on. The MiniDSP re-enumerates and answers about
   22 s later. The board stays on WiFi and keeps serving HTTP.
3. A snapshot restore that loses the MiniDSP this way waits for it, then resumes
   on the new connection from the preset that failed. The presets written before
   the stall are recorded as restored; the restore's writes are absolute values,
   so redoing a partly written preset is safe. It recovers once per restore.
4. The stall supervisor restarts the board if any endpoint halt still waits past
   30 s, from any task, as a last resort.

Other long write paths don't resume yet: measurement mode, `preset/reset-all`,
ezBEQ loads, and writes through the TCP bridge or `/config`. A stall there still
fails that request, but the board recovers.

## Testing the recovery

- `POST /debug/usb/reset-bus` runs step 2 on demand.
- `POST /debug/usb/stall {"afterCommands": N}` makes the Nth next HID command
  behave as a stall: bus reset, then a timeout. A 2x4 HD restore issues fewer
  than 1,500 commands. With `afterCommands: 600` a restore stopped in preset 2 of 4,
  resumed, and finished in 97 s instead of 65 s; a fresh export was strict and
  matched the baseline exactly.

## Next steps

- Run long snapshot restores over direct USB from a computer. If the MiniDSP
  never stalls there, the trigger is on the ESP32 side (scheduling, timing or
  descriptor-DMA behaviour) and is worth chasing in the host stack. If it stalls
  there too (minidsp-rs retries failed writes up to 10 times, 250 ms apart, which
  could hide it), it's the MiniDSP firmware and recovery is the best we can do.
- Watch for `USB OUT accepted … after the 5s timeout`. If the MiniDSP comes back
  by itself within the grace period, a longer timeout alone avoids the bus reset.
- Make the other long write paths resumable the same way if stalls show up there.
