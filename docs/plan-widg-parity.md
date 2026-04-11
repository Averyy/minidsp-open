# Wi-DG Parity Implementation Plan

**Goal.** Make minidsp-open (both ESP32 and desktop targets) indistinguishable
from a real MiniDSP Wi-DG on the wire, so that the stock MiniDSP Device
Console auto-discovers and controls either of our implementations exactly as
it would a real Wi-DG.

## What this plan is NOT

**This plan is a compatibility / discovery fix, not a control-surface fix.**

It does NOT directly fix any of the DSP control gaps tracked in
`PROGRESS.md` Active Work (DSP100 compressor drift, FIR tap readback,
selector-word authoritative readback, `/play` compatibility edges). Those
gaps are in our own HTTP 5380 + CLI + ezBEQ control surface, and they're
addressed by a separate ongoing workstream that talks directly to the DSP
over USB HID.

What it **does** do — and this is the reason to ship it:

1. **Device Console auto-discovers minidsp-open on any LAN with zero config.**
2. **Device Console can drive the full DSP feature set through our bridge.**
   The Wi-DG is a transparent 64-byte pipe with zero DSP logic — our bridge
   is the same. So whatever features Device Console supports on a real
   Wi-DG, it supports through our bridge. This gives users a supported
   fallback for any feature our own API doesn't cover yet — effectively a
   feature-coverage safety net.
3. **Turns the desktop daemon into a rack-mounted Wi-DG replacement.** Any
   Linux/macOS/Windows box with one or more USB MiniDSPs attached becomes
   an N-device Wi-DG substitute with zero configuration.

The Wi-DG binary reverse-engineering revealed **zero new DSP protocol
knowledge** because the Wi-DG binary has zero DSP protocol logic inside it.
So this plan does not make our own control surface smarter or more complete
— it just makes us compatible with the stock MiniDSP client that already
knows how to drive every edge case.

## Reference docs (read alongside this plan)

- `docs/ref-widg-reimplementation.md` — master behavioral spec (every byte)
- `app/widg-re/beacon.md` — UDP discovery beacon byte layout
- `app/widg-re/tcp-framing.md` — TCP 5333 framing contract
- `app/widg-re/server-fingerprint.md` — server response fingerprints
- `app/widg-re/gap-audit.md` — doc consistency audit
- `app/widg-re/live-verification-plan.md` — hardware test gates

**Current state.** See audit report from 2026-04-10. We're ~65% of the way
to full wire parity. The hard work (USB HID framing, TCP bridge, UDP packet
structure, device identity resolution) is mostly done. The remaining gaps
are byte-level corrections and configuration, not new architecture.

---

## Phases

| Phase | Goal                                                      | Est. effort | Status          |
|-------|-----------------------------------------------------------|-------------|-----------------|
| P0    | Device Console auto-discovers minidsp-open as a Wi-DG     | ~7 h        | **Do first**    |
| P1    | Byte-for-byte fingerprint match (UDP + TCP + mDNS + HTTP) | ~5 h        | Do after P0     |
| P3    | Deliberate security improvements over Wi-DG               | ~2 h        | Parallel / any time |
| P2    | Serve the stock Wi-DG React SPA unmodified (ubus surface) | ~3-5 days   | **Skipped — do our own UI** |
| P4    | Alexa integration                                         | —           | **Skipped — proprietary cloud relay** |

**P0 + P1 + P3 is the whole job.** Total ~14 hours of concrete work on top
of our existing codebase.

### Why P2 is skipped

The Wi-DG's own React SPA (`/www/build/en.app.js` + 2 Chinese bundles) is
purely a **router admin panel** — WiFi provisioning, password change,
firmware upload, reboot, factory reset. It has **zero DSP controls.** Every
DSP feature on the Wi-DG is accessed via Device Console over TCP 5333, not
via the SPA.

We already expose the Wi-DG's "router admin" concerns (WiFi provisioning,
OTA, reboot) through the ESP32 firmware's own minimal UI. We do not need
MiniDSP's version, and re-serving it would inherit a commercial Riona Sans
font license, leftover LinkIt Smart 7688 branding in meta tags, and 3
language bundles we don't maintain.

When we build our own web UI for minidsp-open, it will expose both the
router-admin concerns AND the DSP controls (which the stock Wi-DG SPA can't
do) — a strict superset. P2 work is therefore deferred to whenever we
decide to build that unified UI, and it won't be Wi-DG compat work — it'll
be a fresh design.

The 15 ubus methods + `cgi-upload` contract are fully specified in
`app/widg-re/rpc-surface.md` if we ever change our minds.

### Why P4 is skipped

The Wi-DG's Alexa integration is a 204-line Node daemon that relies on:

- A private MiniDSP-operated WebSocket relay at `wss://dev.earsdb.com/alexa`
- A registered Amazon Alexa Smart Home Skill in MiniDSP's developer account

Neither is reproducible clean-room. The on-device protocol is fully
documented in `app/widg-re/alexa-protocol.md` for posterity, but no
implementation work is planned.

---

## P0 — Device Console auto-discovery (~7 h critical path)

Everything in P0 is required for the stock Device Console to find and
control our firmware. Without all five items, Device Console either
doesn't see the device or fails to identify it.

### P0-1 — UDP discovery beacon byte-level rewrite  ~4 h

**Current state (audited 2026-04-10):**
- `protocol/src/discovery.rs:7` — magic header is `[0x80, 0x00, 0x05, 0xA0]`
- `protocol/src/discovery.rs:53-70` — `DiscoveryPacket::encode()` emits
  `36 + hostname_len` bytes, name field at offset 35
- `daemon/src/discovery/mod.rs:37` — desktop sends every 1 second
- `app-esp32/src/main.rs:547` — ESP32 sends every 1 second

**Target state:** exact 72-byte beacon per `app/widg-re/beacon.md` §1 and
`docs/ref-widg-reimplementation.md` §3.3. 5-second interval.

**Specific changes:**

1. **`protocol/src/discovery.rs`** — rewrite `DiscoveryPacket::encode()`:
   - Fix magic byte 1: `0x00` → `0x12`.
   - Pad to exactly 72 bytes (`memset(0xFF)` baseline, then overwrite fields).
   - Fixed bytes `0x14 0xD5` at offset 0x0C–0x0D (htons(5333) port tag —
     value is mandatory, semantics unconfirmed but we emit the bytes regardless).
   - Field layout per the 20-row table in beacon.md §1. Every byte.
   - Name at offset 0x24, length byte at 0x23, padding `0xFF` to 0x47.

2. **`protocol/src/discovery.rs`** — add `DiscoveryPacket::encode_appliance()`:
   - Returns a 72-byte beacon where `0x12–0x17` and `0x23–0x47` are left
     as `0xFF` (the memset baseline). Magic header + MAC + IP + port tag
     stay populated. This is the no-DSP-attached path.

3. **`daemon/src/discovery/mod.rs`** — change send interval from 1 s → 5 s
   at `broadcast_loop()`.

4. **`daemon/src/discovery/mod.rs`** — iterate over every IPv4 interface
   from `ifaddrs` and rewrite beacon bytes 0x0E–0x11 per interface per
   `sendto` (per Wi-DG's FUN_00401ecc loop behavior). This is the "per
   cycle per 5 s" iteration.

5. **`app-esp32/src/main.rs:udp_discovery()`** — mirror both changes
   (interval + per-interface) and switch to the new 72-byte encoder.

6. **`app-esp32/src/main.rs`** — call `encode_appliance()` when no DSP is
   attached (lines 579–589 area where the fallback currently lives).

**Validation (static):** compare byte-for-byte against beacon.md §1.  
**Validation (live):** TEST-01 from `live-verification-plan.md`.

### P0-2 — ESP32 SoftAP SSID format  ~30 min

**Current state:** `app-esp32/src/wifi.rs:27` — `SOFTAP_SSID = "minidsp-setup"`.

**Target state:** `miniDSP-Wi-DG_XXYYZZ` where `XXYYZZ` is the last 3 MAC
bytes in uppercase hex. See `docs/ref-widg-reimplementation.md` §3.1,
derived from Wi-DG's `/etc/uci-defaults/51_linkit_config` behavior at first
boot reading `/dev/mtd2` offset 0x07 and formatting uppercase-hex.

**Specific changes:**

1. **`app-esp32/src/wifi.rs`** — remove `SOFTAP_SSID` constant.
2. **`app-esp32/src/wifi.rs`** — add `softap_ssid()` helper that reads the
   current WiFi MAC (ESP-IDF `esp_wifi_get_mac` or `esp_read_mac`), takes
   the last 3 bytes, formats as `format!("miniDSP-Wi-DG_{:02X}{:02X}{:02X}", b[3], b[4], b[5])`.
3. Callers get the SSID dynamically at provisioning-mode entry instead of
   at constant-init time.

**Validation:** boot ESP32 in SoftAP mode, confirm the visible SSID matches.

### P0-3 — ESP32 hostname + LAN IP + DHCP config  ~1 h

**Current state:**
- `app-esp32/src/main.rs:401` — hostname `format!("minidsp-{serial}")`
- SoftAP LAN IP: ESP-IDF default `192.168.4.1`
- DHCP pool: ESP-IDF default

**Target state:** Wi-DG spec §3.1 — hostname `miniDSP-Wi-DG`, LAN IP
`192.168.100.1/24`, DHCP `192.168.100.100–192.168.100.249`, 12 h lease.

**Specific changes:**

1. **`app-esp32/src/main.rs`** — replace hostname formatting with fixed
   constant `"miniDSP-Wi-DG"`. Applies to both `esp_netif_set_hostname`
   and mDNS hostname.
2. **`app-esp32/src/wifi.rs`** — in SoftAP init, explicitly set:
   - `ip_info.ip = 192.168.100.1`
   - `ip_info.netmask = 255.255.255.0`
   - `ip_info.gw = 192.168.100.1`
   - via `esp_netif_dhcps_stop` → `esp_netif_set_ip_info` → `esp_netif_dhcps_start`.
3. **`app-esp32/src/wifi.rs`** — configure DHCP pool: start `192.168.100.100`,
   end `192.168.100.249`, lease 12 h. Use `esp_netif_dhcps_option` with
   `ESP_NETIF_SUBNET_MASK`, `ESP_NETIF_ROUTER_SOLICITATION_ADDRESS`, or
   the appropriate lwip DHCPS options.
4. **`app-esp32/src/wifi.rs`** — if the current code stops SoftAP on STA
   connect, change it to keep SoftAP running in dual-mode (matches Wi-DG's
   concurrent AP+STA on `apcli0`).

**Validation:** connect a laptop to the ESP32 SoftAP, `ifconfig` should
show `192.168.100.x`, and `http://192.168.100.1/` should respond.

### P0-4 — TCP 5333 socket options + multi-client write mutex  ~1 h

**Current state:**
- Both desktop and ESP32 bind `0.0.0.0:5333` and listen, without explicit
  `SO_REUSEADDR`, `TCP_NODELAY`, or `SO_KEEPALIVE`.
- Single client at a time (matches Wi-DG).
- No explicit locking around concurrent USB OUT transfers if we ever
  enable multi-client.

**Target state:**
1. Match Wi-DG's `SO_REUSEADDR + TCP_NODELAY` **and** add `SO_KEEPALIVE`
   (our intentional improvement — Wi-DG omits it, which causes its known
   dead-client accept-slot blocking bug).
2. Fan out TCP 5333 to **N concurrent clients** instead of the Wi-DG's
   single-client model. Reads (USB IN → TCP) fan out to every connected
   client unconditionally.
3. Serialize writes (TCP → USB OUT) behind a mutex so only one 64-byte HID
   OUT transfer is in flight at a time. Each client sees its own ACKs
   return in order. Prevents the race condition noted in
   `app/widg-re/tcp-framing.md` §11 where the Wi-DG's DC+Alexa threads
   can theoretically interleave frames on USB.

**Specific changes:**

1. **`daemon/src/tcp/mod.rs`** — before `bind()`, call
   `socket.set_reuse_address(true)`; on each accepted stream,
   `stream.set_nodelay(true)?` + tokio `TcpKeepalive`-based config.
2. **`daemon/src/tcp/mod.rs`** — replace the single-client accept loop with
   an accept loop that spawns a per-client task on each `accept()`. Share
   a `tokio::sync::Mutex<UsbTransferSlot>` (or the existing queue's
   serialization) across all client tasks for the write path. The USB IN
   pump broadcasts to a `tokio::sync::broadcast::Sender<[u8; 64]>` that
   every client subscribes to.
3. **`app-esp32/src/main.rs:tcp_bridge()`** — same three setsockopts via
   esp-idf-sys FFI (`lwip_setsockopt`). Then rewrite the single-client
   pump loop to track a `Vec<TcpStream>` of accepted clients with a
   `Mutex` guarding USB OUT writes. Reads iterate the stream vec and
   `write_all` on each (dropping clients that error on write).
4. **`protocol/`** (shared) — if there isn't already a USB transfer queue
   that serializes command sequencing (there is — the shared `queue.rs`
   already does this for the HTTP/CLI paths), route TCP bridge writes
   through it so we get existing opcode-aware timeout handling for free.
   Otherwise, add a thin mutex in front of `libusb_interrupt_transfer`.

**Validation:**
- `netstat -p tcp -n` shows `REUSEADDR`.
- Wireshark shows `PSH` segments with no delay (`TCP_NODELAY`).
- Idle sockets emit keepalive probes after `tcp_keepalive_time` seconds.
- With 2 clients connected, issuing simultaneous writes from both does not
  interleave on the USB bus (capture via `usbmon` or ESP32 bridge logs).
- With 2 clients connected, both see the same USB IN reads at the same time
  (both Device Console and ezBEQ watching a meter both update in lockstep).

### P0-5 — Desktop daemon UDP beacon + TCP 5333 default-on  ~1 h

**Current state:** `daemon/src/discovery/mod.rs` and `daemon/src/tcp/mod.rs`
exist but only start if enabled in `config.toml`.

**Target state:** Desktop daemon automatically emits UDP 3999 beacons and
listens on TCP 5333 whenever it has at least one USB MiniDSP connected OR
the user opts into appliance mode. Device Console should find a running
desktop daemon with zero config.

**Specific changes:**

1. **`daemon/src/lib.rs`** (or main config type) — flip defaults so that
   `discovery = true` and `tcp_server = { enabled = true, port = 5333 }` are
   the baseline, with config only needed to DISABLE.
2. **`config.example.toml`** — update the example to show these as
   defaulted-on and explain how to disable.
3. **Sanity:** the desktop HTTP API on 5380 stays as-is (that's our own
   API, not Wi-DG compat). The new defaults add the Wi-DG listeners
   alongside it.
4. **Bonus:** if no USB MiniDSP is attached, emit appliance-mode beacons
   (all `0xFF` identity bytes) so the daemon still shows up in Device
   Console's list — useful for "the computer is up but the DSP is
   unplugged" diagnostics.

**Validation:** `cargo run -p minidsp-daemon`, launch Device Console on
the same LAN, confirm the daemon shows up in the device list.

### P0 completion criteria

- [ ] Running `cargo run -p minidsp-daemon` with a 2x4 HD attached → Device
      Console finds it and can read the full config (DSP state, volumes,
      PEQs, presets) over TCP 5333.
- [ ] Running `minidsp-open` on an ESP32 with a 2x4 HD attached over USB
      → Device Console (on a laptop joined to the ESP32's SoftAP or the
      same STA network) finds it and can read the full config.
- [ ] Hexdump of a single emitted beacon exactly matches the 72-byte table
      in `beacon.md` §1 (magic bytes, field offsets, 0xFF padding, MAC, IP,
      hw_id, dsp_no, SN, device name).
- [ ] ESP32 SoftAP SSID matches the regex `^miniDSP-Wi-DG_[0-9A-F]{6}$`.
- [ ] ESP32 SoftAP clients get an IP in `192.168.100.100–.249`.

---

## P1 — Wi-DG fingerprint match (~5 h)

These items are not required for Device Console control, but they make a
real Wi-DG and our firmware indistinguishable to everything on the LAN
short of deep code inspection.

### P1-1 — mDNS hostname + service type  ~1 h

**Current state:** `app-esp32/src/main.rs:382-438` — `start_mdns()`
advertises hostname `minidsp-{serial}` with service type `_minidsp._tcp:5380`.

**Target state:** hostname `miniDSP-Wi-DG`, service type `_http._tcp:80`
with TXT record `path=/`. Service instance name can be anything — we
deliberately skip the `LinkIt Smart7688 WebUI on miniDSP-Wi-DG` fingerprint
because the `LinkIt Smart7688` prefix is a MiniDSP oversight we're not
obligated to reproduce.

**Specific changes:**

1. **`app-esp32/src/main.rs`** — hostname from `minidsp-{serial}` →
   `"miniDSP-Wi-DG"` (shared with P0-3 — make it a single constant).
2. **`app-esp32/src/main.rs:start_mdns()`** — change service type from
   `_minidsp._tcp:5380` → `_http._tcp:80`. Add TXT record `path=/`.
3. **Desktop:** if we have mDNS support there (needs verification), mirror
   the same service advertisement. If we don't, add it via the `mdns-sd`
   crate. Desktop mDNS is optional but nice-to-have.

**Validation:** `dns-sd -B _http._tcp` on macOS should show our device.
`dns-sd -G v4 miniDSP-Wi-DG.local` should resolve. `curl http://miniDSP-Wi-DG.local/`
should hit our HTTP server.

### P1-2 — HTTP→HTTPS redirect on port 80  ~1 h

**Current state:** ESP32 serves HTTP on 80 in SoftAP mode, 5380 in STA. No
HTTPS.

**Target state:** Port 80 responds with `302 Location: https://<host>/` per
Wi-DG behavior. HTTPS on 443 with self-signed ECDSA cert.

**Specific changes:**

1. **`app-esp32/src/http_server.rs`** — add a minimal EspHttpServer on :80
   whose sole handler returns `302` with the `Location` header.
2. **`app-esp32/src/http_server.rs`** — second EspHttpServer on :443 with
   TLS using an ESP-IDF generated or bundled ECDSA-P256 cert
   (`CN=miniDSP-Wi-DG`, validity 3650 d). Cert key pair generated at
   first boot and persisted in NVS; regenerated if missing.
3. **`app-esp32/src/http_server.rs`** — add `Server: miniDSP-Wi-DG` header
   to all responses (intentional branding).

**Note:** We deliberately do NOT replicate Wi-DG's RSA-1024 `L=Uknown`
typo subject. We use ECDSA-P256 with a clean subject line.
See `app/widg-re/server-fingerprint.md` §2.

### P1-3 — Desktop mDNS  ~1 h

Same as ESP32 via `mdns-sd` crate. Only if not already implemented.

### P1-4 — DHCP client hostname option 12  ~30 min

**Current state:** ESP32 doesn't advertise its hostname via DHCP when it
joins a user's STA network. Wi-DG doesn't either — but we should.

**Target state:** Send DHCP option 12 = `miniDSP-Wi-DG` whenever the
ESP32's STA interface requests an address from an upstream router. This
makes the device visible in the router's client list with a meaningful
name.

**Specific changes:**

1. **`app-esp32/src/wifi.rs`** — `esp_netif_dhcpc_option` with
   `ESP_NETIF_HOSTNAME` or equivalent to set the DHCP client hostname.

### P1-5 — Appliance-mode beacon semantic cleanup  ~30 min

**Current state:** `app-esp32/src/main.rs:579-589` sends a fallback beacon
with `hw_id=0, dsp_id=0` when no DSP is present.

**Target state:** Use the new `encode_appliance()` path from P0-1 that
leaves identity bytes as `0xFF`. This matches Wi-DG's behavior of never
populating those fields in appliance mode and is the correct byte-level
match.

**Specific changes:**

1. Rip out the `hw_id=0, dsp_id=0` fallback, call `encode_appliance()`
   instead.

### P1 completion criteria

- [ ] `curl -I http://<device>/` returns `302 Location: https://<device>/`
      with `Server: miniDSP-Wi-DG`.
- [ ] `curl -Ik https://<device>/` returns `200` with `Server: miniDSP-Wi-DG`.
- [ ] TLS cert subject is `CN=miniDSP-Wi-DG`, ECDSA-P256 signature.
- [ ] `dns-sd -B _http._tcp` shows the device; `dns-sd -G miniDSP-Wi-DG.local`
      resolves.
- [ ] Upstream router DHCP log shows the STA interface requesting with
      hostname `miniDSP-Wi-DG`.
- [ ] Hexdump of an appliance-mode beacon has `0xFF` at offsets 0x12–0x17
      and 0x23–0x47.

---

## P2 — Stock Wi-DG React SPA (SKIPPED)

**Decision: not planned.** See rationale in the "Phases" table above. The
Wi-DG's React SPA is purely a router-admin panel with zero DSP controls,
encumbered by a commercial font license, and superseded by our own
minidsp-open web UI (whenever we build one, it'll expose both router-admin
AND DSP controls — a strict superset of the stock Wi-DG SPA).

The full ubus surface, cgi-upload contract, and first-boot password flow
are specified in `app/widg-re/rpc-surface.md` if we ever reverse this
decision.

---

## P3 — Security improvements (~2 h, independent of P0/P1)

Per `docs/ref-widg-reimplementation.md` §8 "Deliberate security improvements"
table. These can happen any time and don't block other phases.

- [ ] SoftAP defaults to **WPA2-PSK** with a printed-on-label password
      (derived from MAC or unit serial). Add an opt-in "open AP" mode for
      users who want strict Wi-DG parity during provisioning.
- [ ] Optional HTTP BasicAuth gate on the management UI (behind a flag).
- [ ] TLS cert ECDSA-P256 (already in P1-2), 3650-day validity, clean subject.
- [ ] Remove `/www/log-minidsp.txt` equivalent from the unauthenticated
      space. Either serve it under auth or don't serve it at all.

---

## P4 — Alexa integration (SKIPPED)

**Decision: not planned.** The device-side protocol is fully documented in
`app/widg-re/alexa-protocol.md` and `docs/ref-widg-reimplementation.md`
§3.9 if we ever revisit this, but the implementation requires a private
cloud relay and a registered Amazon Smart Home Skill that are proprietary
to MiniDSP. Clean-room reimplementation would require us to run our own
cloud infrastructure — not a minidsp-open-style project.

---

## Ordering and dependencies

```
P0-1 UDP beacon rewrite          ──┐
P0-2 ESP32 SoftAP SSID             │
P0-3 ESP32 hostname + LAN IP       ├─── P0 complete → Device Console works
P0-4 TCP options + write mutex     │
P0-5 Desktop daemon default-on   ──┘
                                     │
                                     ↓
P1-1 mDNS hostname + service type  ──┐
P1-2 HTTP→HTTPS redirect + TLS       │
P1-3 Desktop mDNS                    ├─── P1 complete → byte-perfect match
P1-4 DHCP option 12                  │
P1-5 Appliance-mode cleanup        ──┘

P3  Security improvements          ───── independent, any time
```

P0 items can be done in parallel (ESP32-side and desktop-side are largely
independent; only the shared `protocol/src/discovery.rs` encoder is
touched by both sides for P0-1). P1 depends on P0 only for the shared
hostname constant. P3 is independent of everything.

---

## Relationship to the control-surface workstream

This plan is orthogonal to the DSP control-surface work tracked in
`PROGRESS.md` Active Work (DSP100 compressor drift, FIR tap readback,
selector-word authoritative readback, `/play` compatibility edges). Those
items affect our own HTTP 5380 / CLI / ezBEQ API and are unblocked by
direct DSP-over-USB protocol work, not by Wi-DG compatibility.

Running in parallel:

| Workstream                           | Touches                              | Fixes                                           |
|--------------------------------------|--------------------------------------|-------------------------------------------------|
| This plan (Wi-DG parity)             | UDP 3999, TCP 5333, WiFi, mDNS       | Device Console compat, zero-config discovery    |
| `PROGRESS.md` Active Work (DSP)      | Our own HTTP/CLI/ezBEQ, shadow cache | Our own API feature coverage                    |

**Side benefit:** after this plan ships, any feature the DSP control-surface
workstream hasn't closed yet has a supported fallback — users can open
Device Console, point it at our bridge, and drive the feature via MiniDSP's
own client that already knows how to work around every edge case the
hardware has.

---

## Verification gates

After each phase, run the applicable live-hardware tests from
`app/widg-re/live-verification-plan.md`:

- After P0: **TEST-01** (beacon byte capture), **TEST-02** (TCP framing),
  **TEST-06** (USB HID opcode capture on a real Wi-DG for cross-check).
- After P1: **TEST-03** (Device Console discovery channel — confirm UDP only),
  **TEST-04** (beacon byte 0x0C-0x0D semantics).

All tests require a physical Wi-DG on the same LAN plus a laptop running
Wireshark and Device Console.

Additionally, after P0-4 run a multi-client soak test: start both Device
Console AND ezBEQ against the same bridge simultaneously, edit PEQs from
ezBEQ while Device Console is displaying the same output, confirm both
clients see consistent state without frame interleave on the USB bus.

---

## References

- `docs/ref-widg-reimplementation.md` §3 (external contract), §5 (RPC surface), §6 (boot), §8 (checklist)
- `app/widg-re/beacon.md` (UDP beacon byte layout)
- `app/widg-re/tcp-framing.md` (TCP 5333/5334 contract)
- `app/widg-re/rpc-surface.md` (ubus methods)
- `app/widg-re/server-fingerprint.md` (uhttpd/dropbear/TLS/mDNS details)
- `app/widg-re/live-verification-plan.md` (hardware test gates)
- `app/widg-re/gap-audit.md` (cross-doc consistency)
