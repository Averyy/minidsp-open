# Wi-DG Clean-Room Reimplementation Spec

**Scope.** Full behavioral spec for reimplementing the MiniDSP Wi-DG
(firmware v0.9.6, built 2022-08-26) on ESP32-S3 so that the result is
**indistinguishable from a real Wi-DG to every external client**: the stock
MiniDSP Device Console, the Wi-DG web UI (if we serve it), and any LAN
observer running Wireshark.

**What this spec is NOT.** This is a compatibility / discovery / wire-format
reference, not a DSP protocol reference. The Wi-DG binary has zero DSP
protocol logic inside it — it's a transparent 64-byte pipe — so this
reverse-engineering pass revealed no new knowledge about DSP control,
compressor readback, FIR state, or any other DSP feature quirk. DSP
control-surface parity is a separate, ongoing workstream tracked in
[PROGRESS.md](../PROGRESS.md#device-control-and-compatibility). See `docs/plan-widg-parity.md` for the
relationship between these two workstreams and the "Relationship to the
control-surface workstream" section there.

The concrete value of this spec is:

1. **Device Console auto-discovers minidsp-open on any LAN with zero config.**
2. **Target end state: Device Console drives the full DSP feature set through our bridge**
   (because the bridge is intended to be transparent, so once the remaining
   bridge-parity and end-to-end Device Console verification work closes,
   every feature Device Console supports on a real Wi-DG should work
   through us — effectively a feature-coverage safety net for anything our
   own API hasn't implemented yet).
3. **Desktop daemon becomes a rack-mounted Wi-DG replacement** for any
   host running Linux/macOS/Windows with one or more USB MiniDSPs attached.

**Status.** Static RE is complete. Every file in the extracted Wi-DG rootfs
is classified. Every user function in `/IoT/minidsp` is decompiled. Every
rpcd plugin is byte-level Ghidra-audited and confirmed stock (no MiniDSP
patches). Every server response fingerprint (uhttpd, dropbear, TLS cert,
dnsmasq, avahi, sysctl) is documented verbatim. The 17 uci-defaults files,
the Alexa WebSocket schema, and the kernel/DTB layout are all captured. A
final cross-doc gap audit (`app/widg-re/gap-audit.md`) resolved every
STATIC discrepancy. The only residual work is a short list of live-capture
tests in `app/widg-re/live-verification-plan.md` — all either MEDIUM/LOW
priority or confirming-not-discovering findings — that require a physical
Wi-DG to settle.

**Interoperability research.** The implementation is original; this is not a
claim of a separated clean-room process. Findings in this spec and its companion docs are derived
from (a) decompiled pseudocode, (b) raw memory/byte reads, and (c)
human-readable config files that are effectively their own source.

This document (`ref-widg-reimplementation.md`) and `plan-widg-parity.md`
record interoperability behavior in independently written descriptions. This
does not grant rights to vendor expressions or data; see the root `NOTICE`. The detailed RE internals under `app/widg-re/` and
`app/ref-widg-firmware.md` live in the **gitignored `app/` directory** and
are not checked in — they contain verbatim excerpts of MiniDSP's firmware
configs, decompiled C pseudocode, and strings from the extracted image,
and are for local developer reference only.

See `app/ref-widg-firmware.md` for the earlier summary reference (kept
for historical context, local only). **This document is the definitive
rebuild instructions** — everything you need to produce a byte-for-byte
compatible replacement.

**Detailed RE companions (22 files under `app/widg-re/`, local-only):**

- `binary-audit.md` · `beacon.md` · `tcp-framing.md` · `device-table.md` — the `/IoT/minidsp` binary
- `boot-chain.md` · `firmware-image.md` · `etc-complete.md` · `lib-bin-sbin.md` · `usr-tree.md` · `filesystem.md` — boot, kernel, rootfs layout
- `network-stack.md` · `service-identity.md` · `server-fingerprint.md` — network & server fingerprint
- `webui.md` · `www-tree.md` · `rpc-surface.md` · `rpcd-plugins.md` — web UI + management RPC
- `alexa-client.md` · `alexa-protocol.md` — Alexa integration
- `live-verification-plan.md` · `gap-audit.md` — residual confidence gaps + final cross-check

---

## 0. Wi-DG at a glance

The Wi-DG is a LinkIt Smart 7688 reference board (MediaTek MT7628AN, 128 MB
DDR2, 32 MB NOR flash, single-band 2.4 GHz WiFi + USB host) running
OpenWrt Chaos Calmer 15.05.1 (kernel 3.18.91). MiniDSP's total customization
is tiny:

| Surface          | Total MiniDSP-authored content                               |
|------------------|--------------------------------------------------------------|
| Binaries         | 1 × 17 KB MIPS C binary (`/IoT/minidsp`)                      |
| Scripts          | 1 × 9-line shell launcher (`/IoT/init.sh`), 3 uci-default scripts, 1 `rc.local` line |
| Web UI           | 1 React SPA in 3 language bundles (~640 KB each) at `/www/build/` |
| Alexa daemon     | 1 × 204-line Node.js daemon at `/IoT/alexa-client/`           |
| Config           | 1 ACL file (`/usr/share/rpcd/acl.d/linkit_rpc.json`)          |
| Everything else  | **Stock OpenWrt 15.05.1 LinkIt Smart 7688 base image**        |

The rest of the 22 MB firmware image is off-the-shelf OpenWrt packages —
uhttpd, dropbear, rpcd, ubusd, dnsmasq, avahi, hostapd, wpad, iwinfo, Node
v0.12.7, Python 2.7.13, LuCI themes — none of which MiniDSP touched. The
reimplementation can therefore skip everything not listed in §1 below without
changing any external-facing behavior.

---

## 1. The complete MiniDSP footprint (everything we must replicate)

### 1.1 `/IoT/` directory

| Path                            | Kind      | What it is                                                  |
|---------------------------------|-----------|-------------------------------------------------------------|
| `/IoT/init.sh`                  | shell     | Launcher. 9 lines. Launched from `/etc/rc.local` at boot.   |
| `/IoT/minidsp`                  | MIPS ELF  | The 17 KB HID↔TCP bridge. Full decompile in `binary-audit.md`. |
| `/IoT/alexa-client/app.js`      | Node.js   | 204-line Alexa voice integration. Uses `earsdb.com` cloud relay (proprietary MiniDSP-operated Lambda, NOT user-reproducible). |
| `/IoT/alexa-client/public/`     | HTML/JS   | Pairing UI served as `/www/alexa` via symlink.              |
| `/IoT/alexa-client/node_modules/ws/` | Node module | Only npm dependency. Pure-JS WebSocket.                |
| `/IoT/en.app.js`                | stale     | **Dead code.** Old SPA copy, not served. Ignore.            |
| `/IoT/examples/`                | demo      | LinkIt-era GPIO blink demos. Dead code. Ignore.             |

### 1.2 `/etc/` customization (exactly 4 files + 1 line in `rc.local`)

| Path                                | What it does                                                         |
|-------------------------------------|----------------------------------------------------------------------|
| `/etc/uci-defaults/51_linkit_config`| Sets `system.@system[0].hostname=miniDSP-Wi-DG`, `network.lan.ipaddr=192.168.100.1`, `wireless.sta.ssid=miniDSP-Wi-DG_<MAC3>` (last 3 MAC bytes from `/dev/mtd2` offset 7). |
| `/etc/uci-defaults/52_linkit_firewall` | Flips `firewall.@zone[-1].input` from REJECT → **ACCEPT** for the WAN zone. All ports open on the uplink. |
| `/etc/uci-defaults/54_linkit_version` | Stamps `system.@system[0].firmware_version=v0.9.6_miniDSP` into UCI. Readable via the SPA's `uci.get {"config":"system"}` call. |
| `/etc/rc.local`                     | Contains the single line `/IoT/init.sh &` before the stock `exit 0`. |

**Note:** Some prior docs reference `53_linkit_wireless`. It does not exist
in v0.9.6 — the wireless customization is in `51_linkit_config`.

### 1.3 `/www/` customization (React SPA only)

| Path                         | What it is                                         |
|------------------------------|----------------------------------------------------|
| `/www/index.html`            | Bootloader stub for the React SPA. Overwritten at boot from `/www/index_7688.html` by the stock `linkit` init script — the latter is the real source. Head still says `Linkit Smart`; body loads `build/en.app.js`. |
| `/www/zh-cn.html`            | Identical stub but loads `build/zh-cn.app.js`.     |
| `/www/zh-tw.html`            | Identical stub but loads `build/zh-tw.app.js`.     |
| `/www/build/en.app.js`       | 653 KB webpacked React SPA (English strings).      |
| `/www/build/zh-cn.app.js`    | Same SPA, Simplified Chinese strings.              |
| `/www/build/zh-tw.app.js`    | Same SPA, Traditional Chinese strings.             |
| `/www/build/main.css`        | SPA CSS (Riona Sans fonts referenced).             |
| `/www/build/riona-sans-*.otf` | Proprietary MediaTek LinkIt SDK fonts. **License encumbrance — do not redistribute.** Replace with Inter or a system font stack in our reimplementation. |
| `/www/build/7688*.png`       | Board photos. MediaTek branding. Remove or replace. |

Every other file under `/www/` (`index_7688.html`, `webcam/`, `luci-static/`,
`cgi-bin/luci`) is stock LinkIt cruft the SPA never references. Skip.

### 1.4 `/usr/` customization (exactly 1 file)

| Path                                         | What it does                          |
|----------------------------------------------|----------------------------------------|
| `/usr/share/rpcd/acl.d/linkit_rpc.json`      | Grants the authenticated `linkit` role ubus access to the exact method set the SPA calls (see §5). Everything else in `/usr/` is stock rpcd, iwinfo.so, rpcsys.so, cgi-io, Node 0.12.7, Python 2.7.13, LuCI. |

Confirmed at byte level by Ghidra decompile: `iwinfo.so` and `rpcsys.so` are
unpatched stock OpenWrt 15.05.1 rpcd plugins (`app/widg-re/rpcd-plugins.md`).

### 1.5 Nothing else is MiniDSP-authored

Everything in `/lib/`, `/bin/`, `/sbin/`, `/rom/`, `/etc/config/*` (except the
uci-defaults generating it at first boot), and the remaining `/etc/` and
`/usr/` trees is stock OpenWrt 15.05.1 + stock LinkIt Smart 7688 Duo base
image. We do not need to replicate any of it — only its externally visible
behavior.

---

## 2. Firmware image layout

Source: `app/widg-re/firmware-image.md`.

```
Wi-DG_v0.9.6.bin  (22.28 MB — sysupgrade image, firmware partition only)
  0x000000  64 bytes      uImage header (U-Boot legacy format)
  0x000040  1,157,301 B   LZMA-compressed kernel (Linux 3.18.91)
  0x11A9B5  20,975,214 B  SquashFS rootfs (xz-compressed)
  0x14FBAF  149,665 B     0xFF NOR padding
```

Flash MTD partitions (from embedded DTB, not in the .bin):

```
MX25L25635E NOR flash, 32 MB total:
  spi0.0:192k(u-boot)ro,64k(u-boot-env),64k(factory)ro,-(firmware)
  u-boot        192 KB   [ro] bootloader
  u-boot-env     64 KB        env
  factory        64 KB   [ro] MAC + WiFi EEPROM cal data
  firmware    ~31.7 MB        kernel + rootfs (67.7% used); JFFS2 overlay
                              carved from unused tail
```

Factory partition layout (read at first boot):

| Offset   | Size    | Content                                        |
|----------|---------|------------------------------------------------|
| 0x0004   | 6 B     | Ethernet MAC (and source of SSID suffix)       |
| 0x0007   | 3 B     | Last 3 MAC bytes — used as SSID suffix (upper-hex) |
| 0x0028   | —       | DTS reference copy                             |
| 0x0400   | variable| Board-type string                              |
| 0x8000   | variable| WiFi RF calibration EEPROM (MT7628 radio tap)  |

**ESP32 implication.** We do not need to replicate the flash partition
layout. We do need to expose a MAC-derived identifier for the SSID and
beacon. See §3.3.

---

## 3. Externally visible contract (what every LAN client sees)

This is the minimum behavior set to be indistinguishable from a real Wi-DG
on the wire.

### 3.1 WiFi / LAN

| Parameter              | Value                                              |
|------------------------|----------------------------------------------------|
| Default mode           | SoftAP (no STA until user provisions)              |
| SSID                   | `miniDSP-Wi-DG_XXYYZZ` (last 3 MAC bytes uppercase hex) |
| Encryption             | **None (open AP)** — at factory                     |
| Band / channel         | 2.4 GHz, HT40, auto-channel                         |
| LAN IP                 | `192.168.100.1/24`                                  |
| DHCP range             | `192.168.100.100`–`192.168.100.249`, 12 h lease     |
| Captive portal         | **None.** User navigates to `http://192.168.100.1/` manually. No DNS hijacking. |
| LAN DNS name           | `miniDSP-Wi-DG.local` (Avahi-advertised via stock `_http._tcp`) |
| STA mode               | Dual-mode `apcli0` AP-client on MediaTek proprietary wireless stack. After provisioning, the AP remains up and STA joins the user's network simultaneously. |
| STA credential store   | UCI (`wireless.sta.ssid` / `wireless.sta.key`) persisted in the JFFS2 overlay |

ESP32 implementation must replicate the **structural** behavior: same
SSID template format with MAC suffix (but using `miniDSP-open_XXYYZZ`
instead of `miniDSP-Wi-DG_XXYYZZ` for self-identification), open AP by
default, LAN IP `192.168.100.1`, DHCP range, the dual-mode AP+STA
concurrent operation. Our netif hostname is `miniDSP-open` (DHCP option
12, router client list) and our mDNS hostname is `minidsp` (resolves as
`minidsp.local`) — both diverge from Wi-DG's `miniDSP-Wi-DG` label
deliberately.

**Security note: the factory firmware ships with open WiFi AND empty root SSH
password AND WAN input=ACCEPT.** Our reimplementation should fix all three
(WPA2 SoftAP with default password, web UI password change required on first
login, no SSH by default). This is a deliberate, documented deviation from
Wi-DG parity — parity on insecurity is not parity we want.

### 3.2 Open ports (steady state)

| Port       | Proto | Bind     | Purpose                             |
|------------|-------|----------|-------------------------------------|
| **22**     | TCP   | 0.0.0.0  | Dropbear SSH. Root, empty password. |
| **80**     | TCP   | 0.0.0.0  | uhttpd management. Redirects to 443. |
| **443**    | TCP   | 0.0.0.0  | uhttpd HTTPS (self-signed 1024-bit RSA cert). |
| **5333**   | TCP   | 0.0.0.0  | `/IoT/minidsp` Device Console bridge. |
| **5334**   | TCP   | 0.0.0.0  | `/IoT/minidsp` Alexa client bridge.  |
| **8080**   | TCP   | 0.0.0.0  | `alexa-client` re-link trigger HTTP. **NO AUTH** — any LAN host can unlink the device. Security hole. |
| **53**     | UDP   | 0.0.0.0  | dnsmasq DNS.                         |
| **67**     | UDP   | 0.0.0.0  | dnsmasq DHCP.                        |
| **3999**   | UDP   | —        | `/IoT/minidsp` **broadcast only** — not bound for input. |
| **5353**   | UDP   | 0.0.0.0  | Avahi mDNS.                          |

**Required for Device Console parity:** 5333, 3999.
**Required for the web UI:** 80, 443, 53, 67.
**Optional (skip for Phase 1):** 22 (SSH), 5334, 8080 (Alexa).

### 3.3 UDP discovery beacon (port 3999, broadcast, every 5 s) — 72 BYTES

The single most important byte-level contract. See `app/widg-re/beacon.md`
for the full derivation. ESP32 must produce a 72-byte payload **identical**
to the Wi-DG's to be recognized by stock Device Console.

```
offset  width  field           value / source
------  -----  --------------  -----------------------------------------
0x00    1      magic0          0x80  (fixed)
0x01    1      version         0x12  (fixed — semantics unknown, mirror it)
0x02    1      magic2          0x05  (fixed)
0x03    1      magic3          0xA0  (fixed)
0x04    2      pad             0xFF 0xFF  (memset only)
0x06    6      mac             MAC of the WiFi STA interface (apcli0 on Wi-DG)
0x0C    2      port_tag        0x14 0xD5  — value confirmed hardcoded via two separate SB
                               instructions; interpreted as htons(5333) (DC TCP port), but
                               semantics are MEDIUM confidence pending TEST-04 live capture.
                               Emit as-is regardless of interpretation.
0x0E    4      ip              IPv4 of the interface being broadcast FROM
                               (network byte order / big-endian).
                               Wi-DG rewrites this per interface per sendto.
0x12    1      hw_id           HID `02 31 33` response byte [4]
0x13    1      hw_id_sub_a     HID hw_id response byte [2]
0x14    1      hw_id_sub_b     HID hw_id response byte [3]
0x15    1      dsp_no          HID `05 05 FF A1 01 AB` response byte [4]
0x16    1      sn_high         HID `05 05 FF FE 02 09` response byte [4]
0x17    1      sn_low          HID SN response byte [5]
0x18    11     pad             0xFF × 11
0x23    1      name_len        strlen(device_name)
0x24    ≤30    device_name     USB product string (primary),
                               or table fallback by (hw_id, dsp_no) — see §4.
0x24+n  —      pad             0xFF to offset 0x47
                               (last byte at 0x47, total 72 bytes)
```

**Send loop.** Socket: AF_INET/SOCK_DGRAM with `SO_BROADCAST=1`. For each
AF_INET interface returned by `getifaddrs`, rewrite bytes 0x0E–0x11 with that
interface's IP, then `sendto(sock, beacon, 0x48, 0, ifa_broadaddr, 0x10)`.
`sleep(5)` between iterations. **One cycle per 5 s, not one beacon.**

**Appliance-mode (no DSP attached).** Leave bytes 0x12–0x17 and 0x23–0x47 as
`0xFF` (the memset baseline). Device Console's discovery check is only on the
magic header `d[0]==0x80 && d[2]==0x05 && d[3]==0xA0`, so an ESP32 running
minidsp-open with no DSP still shows up as a Wi-DG in the client's device
list — useful for provisioning and status.

### 3.4 TCP bridge (port 5333) — raw 64-byte pipe

From `app/widg-re/tcp-framing.md`. The Wi-DG applies **zero** framing
transforms. Both directions are a single `libusb_interrupt_transfer(0x40)`
paired with `recv(0x40)` or `write(0x40)`. No report-ID byte added or
stripped. No length prefix. No CRC.

```
Listen: bind(0.0.0.0, 5333), SO_REUSEADDR, TCP_NODELAY, listen(backlog=5)
Concurrency: 1 client at a time. accept() blocks until inner recv loop exits.

TCP → USB:
  recv(client_fd, buf, 64, 0)
  libusb_interrupt_transfer(dev, OUT_ep, buf, 64, &n, 5000)

USB → TCP:
  libusb_interrupt_transfer(dev, IN_ep, buf, 64, &n, 5000)
  write(client_fd, buf, 64)   ← fan out to every connected client
```

**ESP32 deviations from Wi-DG (intentional improvements):**

1. **Add `SO_KEEPALIVE`.** Wi-DG omits it; a silently dead DC connection
   blocks the accept slot forever.
2. **Fan out to N clients.** Wi-DG is limited to 1 DC + 1 Alexa client. ESP32
   should allow any number of concurrent Device Console clients.
3. **Serialize writes behind a mutex.** Wi-DG has no locking on concurrent
   USB OUT transfers — its DC and Alexa threads can theoretically interleave
   bytes on the USB bus (see `tcp-framing.md` §11). We add a write mutex so
   only one 64-byte HID OUT transfer is in flight at a time.
4. **Bounded retry on `LIBUSB_ERROR_TIMEOUT`.** Wi-DG busy-loops; add a yield.
5. **Error recovery without process exit.** Wi-DG's entire retry logic is at
   `main()` level (device re-enumeration). ESP32 does not have a process
   model — handle errors in-thread with `usbh_reset_dev` + reopen.

Everything else matches byte-for-byte.

### 3.4.1 State model — the bridge is stateless; the DSP is canonical

The Wi-DG does **zero** DSP state tracking. It holds no config cache, does
no version negotiation, issues no handshake, and has no "does the client's
saved config match the device" check. Every PEQ coefficient, every XO
crossover, every preset, every compressor parameter lives in the DSP's
flash and RAM. The bridge transports bytes; the DSP is the single source
of truth.

What Device Console does on each connect (through a real Wi-DG or our
bridge — identical in both cases):

1. Read `hw_id`, `serial`, `dsp_no`, firmware version (direct HID queries,
   passed through transparently).
2. Read live DSP state from RAM — all PEQs, XO, levels, current preset
   index.
3. Read per-preset modification tokens (opcodes `0x06`/`0x07`).
4. Display whatever it just read.

There is no `sync` step, no `push cached state to bridge`, no
`bridge-confirms-session-version`. Device Console trusts the DSP every
time it connects and re-reads from scratch.

**Our bridge has the same contract: hold zero DSP state, forward bytes.**
The shadow-state cache in `daemon/` is local to our own HTTP 5380 /
ezBEQ / CLI API — it is never exposed over TCP 5333, it is never sent to
Device Console, and it never influences the bridge's byte forwarding. If
we delete it the Wi-DG bridge keeps working exactly the same.

**Identity stability.** The three identity values (hw_id, dsp_no, serial)
all come from the attached DSP's flash, not from the bridge. If a user
hot-swaps the same physical 2x4 HD between a real Wi-DG and our bridge,
Device Console sees the same identity bytes on both sides and cannot tell
the difference.

**Concurrent-client consistency.** With the write mutex added in P0-4
(see `plan-widg-parity.md`), two clients (e.g. Device Console + ezBEQ)
can talk to our bridge simultaneously with these semantics:

- **Reads fan out.** Every USB IN (64-byte HID report) is broadcast to
  every connected client. Both clients see the same meter updates, the
  same post-write ACKs, the same state.
- **Writes serialize.** Only one TCP → USB OUT transfer in flight at a
  time. Clients' writes interleave at the frame boundary (not the byte
  boundary), matching the MiniDSP protocol's request/response assumption.
- **No lock held across reads.** Read fan-out is independent of the write
  path.
- **Staleness is handled at the protocol layer.** Each client sends its
  own read-back commands after writes. If client A mutates a PEQ while
  client B is displaying the same PEQ, client B sees the new value on its
  next read — same as the "another MiniDSP app is running in the
  background" case Device Console already tolerates.

This is strictly better than the Wi-DG's behavior. The Wi-DG has no
locking, so its DC and Alexa threads race on USB OUT. In practice the
race is benign because the two clients send non-overlapping command sets
(Alexa only writes volume/mute/preset; DC writes everything else), but
it's a latent bug. We close it.

### 3.5 TCP Alexa bridge (port 5334) — identical protocol, optional

Same code path as 5333. Skip for Phase 1. If reimplemented later, note the
Alexa client itself is unreproducible because it uses MiniDSP's private
`earsdb.com` relay (`app/widg-re/alexa-client.md`).

### 3.6 mDNS (Avahi)

The Wi-DG advertises **three stock services** — `_http._tcp:80`,
`_ssh._tcp:22`, `_smb._tcp:445` — all pointing at hostname
`miniDSP-Wi-DG.local`. **There is no `_minidsp._tcp` service.** Device Console
discovery is UDP 3999 broadcast only; mDNS plays no role in it.

**ESP32 requirement.** Publish `_http._tcp:80` (or :443) with TXT record
`path=/` at mDNS hostname `minidsp` (resolves as `minidsp.local`). Skip
`_ssh._tcp` and `_smb._tcp`. This is sufficient for the web UI to be
reachable via `.local` names.

**mDNS service-name fingerprint.** The Wi-DG's Avahi service files name the
instances as literal strings containing `"LinkIt Smart7688"`:

```
"LinkIt Smart7688 WebUI on miniDSP-Wi-DG"
"LinkIt Smart7688 SSH on miniDSP-Wi-DG"
```

A client that fingerprints mDNS by instance name can distinguish a real Wi-DG
from most alternatives by looking for the `LinkIt Smart7688` prefix. **ESP32
decision:** we will NOT copy this fingerprint (using `miniDSP-open` as the
mDNS instance name instead) because (a) the `LinkIt Smart7688` prefix is a
brand reference MiniDSP never cleaned up, and (b) Device Console does not
parse mDNS at all (discovery is UDP 3999 only). A future scanner built
specifically to detect "is this a real Wi-DG" would fail on our firmware —
this is acceptable and arguably desirable for branding clarity.

### 3.7 Server fingerprint details (uhttpd / dropbear / TLS)

Source: `app/widg-re/server-fingerprint.md`. Byte-level details for every
LAN-observable server on the Wi-DG. None of these affect Device Console
compatibility; they only affect deep fingerprint matching by LAN scanners.

**uhttpd (TCP 80 + 443):**
- Binary: `/usr/sbin/uhttpd` (stock OpenWrt CC 15.05.1 — no version string
  embedded in the binary, reports itself only as `uhttpd`).
- **Sends no `Server:` HTTP response header.** Responses have no
  identifying header.
- TCP 80 always `302 Location: https://...` redirects to TCP 443.
- `/www/index.html` is a `<meta refresh>` to `/cgi-bin/luci` (stock LuCI
  stub), but the SPA is loaded directly by the bootloader stub on first
  load — users never see the LuCI page.
- `max_connections=100`, `max_requests=3`, `network_timeout=30`,
  `http_keepalive=20`, `tcp_keepalive=1`.
- ubus endpoint at `/ubus`, socket `/var/run/ubus.sock`.
- cgi-bin at `/www/cgi-bin/` (contains `cgi-upload`, `cgi-download`
  symlinks to `/usr/libexec/cgi-io` + the LuCI Lua shim).

**TLS cert (generated at first boot, preserved across sysupgrade):**
- Generator: stock OpenWrt `px5g` helper.
- **RSA-1024** (not 2048 — 1024-bit is a modern weakness but matches Wi-DG).
- Self-signed, validity 730 days.
- **Subject:** `C=ZZ, ST=Somewhere, L=Uknown, O=OpenWrt, CN=OpenWrt`
  (note the `L=Uknown` typo — this is a distinctive fingerprint).
- Issuer = subject (self-signed).
- SAN: none.
- Signature algorithm: `sha256WithRSAEncryption`.
- DER-encoded on disk.

**Dropbear SSH (TCP 22, default on in factory firmware):**
- Binary: `/usr/sbin/dropbear` — version string from `strings`:
  `SSH-2.0-dropbear_2017.75` (this is the protocol banner a client sees).
- Host key: **RSA-1024**, generated at first boot by the stock
  `/etc/init.d/dropbear` script, persisted in the JFFS2 overlay, preserved
  across sysupgrade.
- Root login + password authentication ON. **Empty root password** at
  factory. First-boot web UI forces the user to set a password.
- No `BannerFile` set — no text banner sent after the protocol banner.

**dnsmasq (TCP 53 + UDP 53 + UDP 67):**
- Lease time: `12h` (`/etc/config/dhcp` `option leasetime 12h`).
- DHCP options offered: standard set (1 netmask, 3 router, 6 DNS, 28
  broadcast, 51 lease time, 54 server ID). No PXE/TFTP.
- DNS domain: `lan` (default OpenWrt).
- Authoritative for `.lan`, forwards everything else.

**Kernel sysctl non-defaults** (`/etc/sysctl.conf`):
- `net.ipv4.tcp_fin_timeout=30`
- `net.ipv4.tcp_keepalive_time=120`
- `net.netfilter.nf_conntrack_max=16384`
- `net.netfilter.nf_conntrack_tcp_timeout_established=7440`
- ECN explicitly off.
- **No TTL override** — default IPv4 TTL remains 64.

**DHCP client hostname advertisement:** **None.** The netifd/udhcpc client
does NOT advertise DHCP option 12 (hostname) unless explicitly configured,
and the Wi-DG's UCI config does not configure it. Upstream routers that log
DHCP client hostnames will see no name for the Wi-DG.

**ESP32 deviations (intentional, for security and self-identification):**
- Mint a fresh ECDSA-P256 TLS cert with subject `CN=miniDSP-open`. Do NOT
  replicate the `L=Uknown` typo or the 1024-bit RSA key.
- Skip SSH entirely (no Dropbear on ESP32).
- DHCP client should advertise hostname `miniDSP-open` via option 12.
- Send `Server: miniDSP-open` HTTP header for explicit identification.
- mDNS hostname is `minidsp` (resolves as `minidsp.local`) — distinct
  from the netif/DHCP/TLS identity so the user-facing URL stays short.

### 3.8 USB HID boot-time queries (required to fill beacon)

Three HID queries sent on boot, each 64-byte OUT + 64-byte IN, 5 s timeout,
5 retries with 1 s sleep between attempts:

| Query   | TX bytes (rest zero-padded to 64)      | Extraction                                    |
|---------|----------------------------------------|-----------------------------------------------|
| hw_id   | `02 31 33`                              | `resp[4]→hw_id`, `resp[2]→sub_a`, `resp[3]→sub_b` |
| SN      | `05 05 FF FE 02 09`                     | `resp[4]→sn_high`, `resp[5]→sn_low`            |
| dsp_no  | `05 05 FF A1 01 AB`                     | `resp[4]→dsp_no`                               |

Opcode `0x05` is the flash-page read primitive; page `0xFF` holds settings
including SN at offset `0xFE` (2 bytes) and dsp_no at offset `0xA1` (1 byte).
Opcode `0x31` is the dedicated hw_id query.

`app/widg-re/beacon.md` §2 has the full pseudocode including the response
validation logic and retry conditions.

### 3.9 Alexa TCP bridge + cloud relay (optional, Phase 2+)

Full detail in `app/widg-re/alexa-protocol.md`. This section is a summary.

The Wi-DG's Alexa integration is a 204-line ES5 Node.js daemon
(`/IoT/alexa-client/app.js`, Node v0.12.7) that:

1. **WebSocket-connects** to `wss://dev.earsdb.com/alexa` with two custom
   HTTP Upgrade headers: `alexa-id: <linkKey>` and `auth-key: <authKey>`.
   Both are stored in `/IoT/alexa-client/status.json`.
2. **TCP-connects** to `127.0.0.1:5334` (the Wi-DG's own Alexa bridge port)
   with a 2-second reconnect loop.
3. **UDP-binds** `127.0.0.1:3999` to passively snoop the UDP discovery
   beacon and extract the device type from the beacon's name field.
4. **HTTP-listens** on `0.0.0.0:8080` (no auth) for a re-link trigger URL
   that clears the auth token and restarts the WS handshake.

**WebSocket message schema (only 9 types total):**

Outbound: one type — `{"type":"RequestId"}` sent every 2 s as a keepalive /
request-for-work.

Inbound (from earsdb relay):

| Type             | Action                                                      |
|------------------|-------------------------------------------------------------|
| `linkKey`        | Store key in `status.json`, display on pairing UI          |
| `authKey`        | Store token in `status.json`, used as WS Upgrade header    |
| `clearLinkKey`   | Wipe `status.json` and reconnect                           |
| `mute`           | Send `03 17 01 1B` to 127.0.0.1:5334                        |
| `unmute`         | Send `03 17 00 1A` to 127.0.0.1:5334                        |
| `volumeUp`       | Query current volume via `05 05 FF DA 02 E5`, decrement raw byte by 10 (Wi-DG volume is inverted — lower = louder), clamp to 0, send `03 42 <vol> <crc>` |
| `volumeDown`     | Same but increment by 10, clamp to 255                      |
| `setPreset`      | Send `04 25 <preset 0..3> 01 <crc>`                         |

All 5 DSP commands use the standard MiniDSP HID framing
`[length][payload…][crc]` with 8-bit checksum = `sum(bytes[0..length-1]) & 0xFF`.

**Pairing flow:**

1. User opens `http://miniDSP-Wi-DG.local/alexa/` (symlink to
   `/IoT/alexa-client/public/`).
2. SPA fetches `status.json` and polls until `linkKey` appears.
3. `earsdb.com` pushes a `linkKey` via the WS.
4. SPA shows the key; user speaks it to Alexa ("Alexa, my code is ...").
5. Amazon → earsdb cloud → earsdb pushes `authKey` via WS.
6. SPA shows "paired" and stores `authKey` in `status.json` permanently.

**Hard blockers to reimplementation:**

- `earsdb.com` is a private MiniDSP-operated relay. The WS protocol is
  documented above but the server side — authentication against
  `*.earsdb.com`, translation from Amazon Smart Home directives to the WS
  message types, maintaining an Alexa skill registration — is entirely
  proprietary.
- No clean-room substitute unless we register our own Alexa Smart Home skill
  + operate our own relay.

**Security flaws in the stock implementation (do NOT copy):**

- `authKey` persisted at `/IoT/alexa-client/status.json` is served
  unauthenticated at `http://<device>/alexa/status.json` (symlink). Any LAN
  client can steal the pairing token.
- HTTP port 8080 re-link trigger has no auth — any LAN client can unlink.
- WebSocket `JSON.parse` has no try/catch — malformed cloud message crashes
  the daemon.
- No process supervision — crash = dead until reboot.

**Phase 1 ESP32 decision:** Skip entirely. Document as deprecated parity.
If Alexa is ever needed, it requires a new cloud relay + new Amazon Smart
Home skill registration — the device-side protocol is fully spec'd in
`alexa-protocol.md` and can be reimplemented in hours on top of an existing
TCP 5334 client.

---

## 4. Device identification

### 4.1 Resolution order

1. **USB product string** via `libusb_get_string_descriptor_ascii`. First 30
   bytes (`strncpy(…, 0x1e)`). If non-empty, this is the beacon's device-name
   verbatim. This path wins for every normal MiniDSP device.
2. **Table fallback.** If the product string is empty/absent, do a linear
   scan of a 24-entry table keyed on `(hw_id, dsp_ver)` and use the matching
   name.

### 4.2 Table at `0x004140e4` (24 entries × 32 bytes)

Per entry: `{ uint8_t hw_id; uint8_t dsp_ver; char name[30]; }`. **No
collisions** — every `(hw_id, dsp_ver)` pair is unique.

See `app/widg-re/device-table.md` §2 for the full table. Key pairs:

```
(1, 50)  MiniDSP-2x8              (10, 100) MiniDSP-2x4-HD
(1, 51)  MiniDSP-8x8              (10, 101) DDRC-24
(3, 52)  C-DSP-6x8                (11, 97)  C-DSP-8x12
(4, 60)  OpenDRC-2x2              (12, 102) HA-DSP
(4, 66)  MiniSharc-4x8            (12, 104) HA-DSP-CF
(4, 67)  MiniSharc-4x8-96k        (14, 105) SHD
(5, 61)  DDRC-22                  (27, 100) miniDSP Flex
(6, 94)  DDRC-88                  (27, 101) miniDSP Flex DL
(6, 95)  DDRC-88-BM               (30, 110) miniDSP Flex 8
(7, 94)  nanoAVR-DL               (30, 111) miniDSP Flex 8 DL
(8, 96)  nanoAVR-BM               (31, 113) miniDSP Flex HT
```

**ESP32 implication.** Do **NOT** copy this table verbatim into
minidsp-open. `docs/ref-devices.md` is our canonical device registry; use
the Wi-DG table as supplementary evidence to add confirmed `dsp_ver` values
for OpenDRC-2x2 (60), MiniSharc-4x8 (66), MiniSharc-4x8-96k (67), HA-DSP
(102), HA-DSP-CF (104), nanoAVR-DL (94), nanoAVR-BM (96), DDRC-22 (61).
These additions are **flagged for human review** in `device-table.md` §8.

### 4.3 USB VID/PID accept list (12 entries)

Table at `0x004143e4`. Fixed set of MiniDSP devices the Wi-DG recognizes:

```
0x04D8:0x003F  Microchip PIC HID bootloader (firmware update mode)
0x2752:0x0040  DDRC-22
0x2752:0x0041  (reserved, no name-table entry)
0x2752:0x0042  (reserved, no name-table entry)
0x2752:0x0043  DDRC-88 family
0x2752:0x0044  DDRC-24 / Flex DL
0x2752:0x0045  SHD family
0x2752:0x0011  2x4 HD / Flex
0x2752:0x0013  HA-DSP
0x2752:0x0012  Flex Eight
0x2752:0x004A  Flex Eight DL
0x2752:0x004B  Flex HT / Flex HTx
```

**Wi-DG v1.12 predates** the Flex HTx (hw_id 32), SHD Studio, SHD Power,
CDSP-8x12 v2, Harmony DSP. These devices will match by VID/PID but fall
through the name lookup — the beacon then relies on the USB product string,
which every real MiniDSP provides.

---

## 5. Web UI / RPC surface (20 ubus methods)

Source: `app/widg-re/rpc-surface.md` — which contains the definitive
method-by-method contract. Summary only here.

### 5.1 Listener

- `uhttpd` on TCP 80 and TCP 443.
- TCP 80 → `302 Location: https://...` redirect.
- TCP 443 → self-signed 1024-bit RSA cert (`/etc/uhttpd.crt`, `/etc/uhttpd.key`),
  generated at first boot, preserved across sysupgrade.
- docroot: `/www/`. index: `index.html`.
- cgi-bin: `/www/cgi-bin/`.
- ubus endpoint: `POST /ubus` handled by a built-in uhttpd module.

### 5.2 Complete ubus method list (what the SPA actually calls)

```
session.login       login with username+password, returns token
session.grant       grant ACL to the session (stock luci flow)

system.board        returns board_name, model, release, kernel, uptime, etc.

uci.get             read a config section
uci.set             write a config option
uci.apply           atomic apply pending changes
uci.commit          commit staged changes to disk
uci.reload_config   reload daemons using a config file

network.interface   status   per-interface ip, up, device, route
iwinfo.scan         scan for visible APs (STA mode provisioning)

rpc-sys.password_set  change root password via /usr/bin/passwd pipe
rpc-sys.reboot        sync + reboot(LINUX_REBOOT_CMD_RESTART)
rpc-sys.factory       execl("/sbin/jffs2reset", "-y", "-r")
rpc-sys.upgrade_test  execv("sysupgrade", ["--test", "/tmp/firmware.bin"])
rpc-sys.upgrade_start execv("/sbin/sysupgrade", ["/tmp/firmware.bin"])
```

Plus two HTTP CGI endpoints (not ubus):
- `POST /cgi-bin/cgi-upload` — multipart upload → writes to a temp file
  (typically `/tmp/firmware.bin`). Handled by stock `/usr/libexec/cgi-io`.
- `GET /cgi-bin/cgi-download` — same binary, download side. Not used by SPA.

### 5.3 First-boot detection

SPA tries `session.login("root", "")`. If it succeeds, an empty root password
is installed (factory state) and the SPA shows a password-setup screen. On
success it calls `rpc-sys.password_set` and then `session.login` with the new
password.

### 5.4 Firmware upgrade flow

```
1. SPA: POST /cgi-bin/cgi-upload
       multipart: sessionid=<token>, filename=/tmp/firmware.bin, filedata=<binary>
   Response: 200 OK (JSON)

2. SPA: POST /ubus
       rpc-sys.upgrade_test   — runs `sysupgrade --test /tmp/firmware.bin`
   Response: stdout + stderr + exit code

3. If exit code == 0:
   SPA: POST /ubus
       rpc-sys.upgrade_start  {"keep":1}  — runs `sysupgrade /tmp/firmware.bin`
   Response: 200 OK (process detaches; device reboots during upgrade)
```

### 5.5 Localization

Three separate webpack bundles at `/www/build/`. No server-side locale
detection — the SPA uses `window.location.href` to switch to
`/zh-cn.html` / `/zh-tw.html` / `/`. All three bundles are ~640 KB and
behaviorally identical.

### 5.6 Session / auth

- Token returned by `session.login` is passed in subsequent `POST /ubus`
  bodies.
- Logout is **purely client-side** (localStorage wipe). No `session.logout`
  call.
- No CSRF tokens, no CORS headers, no rate limiting.

### 5.7 ACL

`/usr/share/rpcd/acl.d/linkit_rpc.json` grants the authenticated session
access to exactly the methods above. No `file.exec`, no raw shell, no UCI
sections outside the ones the SPA writes.

---

## 6. Boot sequence (what happens from power-on to beacon)

1. **U-Boot** (NOR 0x000000–0x030000) loads the uImage from `firmware`
   partition at 0x50000, decompresses LZMA to `0x80000000`, jumps to kernel.
2. **Kernel (Linux 3.18.91)** brings up UART (`ttyS2,57600`), USB host
   (`ehci-hcd`, `ohci-hcd`, `usbcore`, `usbhid`), MediaTek wireless
   (`mt_wifi.ko` — monolithic, no external firmware).
3. **procd** starts, mounts squashfs rootfs, overlays JFFS2 overlay on top.
4. **`/etc/inittab`** → `/sbin/init` → `/etc/init.d/rcS S boot`. 22 stock
   scripts in S## order: `S00sysfixtime`, `S10boot`, `S10system`, `S10udev`,
   `S11sysctl`, `S12log`, `S19dnsmasq`, `S19firewall`, `S20network`,
   `S50cron`, `S50dropbear`, `S50uhttpd`, `S60avahi`, `S80mjpg-streamer`,
   `S80ntpd`, `S80ubus`, `S85mountd`, `S90led`, `S95done`, etc.
5. **`S95done`** runs `/etc/rc.local` which forks `/IoT/init.sh &`.
6. **`/IoT/init.sh`** (9 lines verbatim):
   ```sh
   #!/bin/sh
   killall minidsp
   sleep 30
   chmod +x /IoT/minidsp
   rm /www/log-minidsp.txt
   /IoT/minidsp > /www/log-minidsp.txt &
   rm /www/log-alexa.txt
   node /IoT/alexa-client/app.js > /www/log-alexa.txt &
   ln -s /IoT/alexa-client/public /www/alexa
   ```
   The **30-second sleep** is a crude wait-for-USB-enumeration hack. There is
   no procd dependency, no hotplug trigger, no supervisor. If `minidsp`
   crashes it stays dead until reboot. `/www/log-minidsp.txt` and
   `/www/log-alexa.txt` are **unauthenticated public HTTP endpoints** — a
   deliberate-or-accidental debugging convenience.
7. **`/IoT/minidsp`** starts: prints `"version 1.12"`, calls `libusb_init`,
   scans network interfaces via `getifaddrs` (`FUN_00401bf4`), enumerates
   USB devices, matches VID/PID against the 12-entry table, opens the first
   match, reads the product string, `libusb_claim_interface`, **sleeps 10
   more seconds** (hardcoded USB stabilization delay), runs the info-gather
   (`FUN_00401294`) — three HID queries + device-name lookup + builds the
   72-byte beacon on `main()`'s stack — and finally `pthread_create`s the
   four worker threads (USB read pump, TCP 5333 accept, TCP 5334 accept, UDP
   3999 broadcast).

Total boot-to-first-beacon time: ≈ 45–55 seconds. Most of that is the 30-second
init.sh sleep plus the 10-second libusb stabilization.

**ESP32 target.** With TinyUSB host + lwip + existing minidsp-open control
surface, we should comfortably hit boot-to-first-beacon under 10 seconds.

---

## 7. Residual unknowns (the "live verification" list)

From `app/widg-re/live-verification-plan.md`. Every finding in this spec is
HIGH-confidence static RE except these items, which require a physical Wi-DG
on the network to fully settle:

| # | Item                                                     | Confidence | Why it matters |
|---|----------------------------------------------------------|------------|----------------|
| 1 | Beacon byte `0x01 = 0x12` — fixed protocol version?      | MEDIUM     | Might be ignored by Device Console, or might be a required version check. Live capture resolves. |
| 2 | hw_id sub-bytes at beacon `0x13, 0x14` — what do they mean? | MEDIUM | Likely chip revision sub-IDs. Mirror whatever the attached DSP returns. |
| 3 | Serial number byte order at `0x16–0x17` — big or little endian? | MEDIUM | High byte first based on response order, not verified to Device Console display. |
| 4 | Does Device Console require the UDP beacon every 5 s, or is one beacon ever enough? | MEDIUM | Capture the first-time device-add flow in Device Console. |
| 5 | Does Device Console ever try mDNS SD or is 3999 the only discovery channel? | LOW | Our static finding: 3999 only. Confirm with pcap. |
| 6 | TCP disconnect handling: after a DC client closes, how soon does the Wi-DG accept the next connection? | LOW | Affects reconnect UX. |
| 7 | 30-second init.sh sleep + 10-second minidsp sleep — does the first beacon go out at T+45s? | LOW | Just a timing confirmation. |

None of these block an ESP32 reimplementation — they only affect whether we
get the Wi-DG's behavior **exactly** right or just **almost** right. For
Phase 1 we can mirror the static-RE-derived values and fix any discrepancies
when we next have a real Wi-DG on the bench.

---

## 8. ESP32 reimplementation checklist

This is the order of operations to produce an indistinguishable-on-the-wire
ESP32 Wi-DG replacement. Items marked `[done]` already exist in
minidsp-open.

### Core (required for Device Console parity)

- [ ] WiFi SoftAP with `miniDSP-open_XXYYZZ` SSID template (open AP — factory
      default — with an ESP32-side opt-in to upgrade to WPA2-PSK).
- [ ] WiFi STA provisioning via the management web UI with UCI-equivalent
      persistence (NVS).
- [ ] LAN IP `192.168.100.1/24`, DHCP server `.100–.249`, 12 h lease.
- [ ] Netif hostname `miniDSP-open` (DHCP option 12 / router client list).
- [ ] mDNS hostname `minidsp` (resolves as `minidsp.local`).
- [ ] mDNS advertise `_http._tcp:80` with TXT `path=/`.
- [done] USB host with MiniDSP VID/PID filter (12-entry table from §4.3).
- [done] USB HID interrupt transfer OUT/IN at 64 bytes.
- [ ] Boot-time HID info gather: 3 queries, exact bytes from §3.7, 5 retries,
      1 s sleep between, 5 s per-transfer timeout.
- [ ] 72-byte UDP discovery beacon — bytes exactly per §3.3. Broadcast every
      5 s from the STA interface when provisioned, from the SoftAP interface
      always.
- [done] TCP 5333 listener with raw 64-byte pass-through in both directions,
      `TCP_NODELAY`, `SO_REUSEADDR`. Add `SO_KEEPALIVE` which the Wi-DG omits.
      Fan out USB IN reads to N connected clients (not limited to 1).
- [ ] Appliance-mode beacon (no DSP): keep magic bytes, leave device identity
      and name as `0xFF`.

### Management UI

- [ ] uhttpd-equivalent HTTP server on 80 (→ 443 redirect) and 443
      (self-signed cert).
- [ ] Serve `index.html` + `build/en.app.js` (or our own replacement SPA).
- [ ] `POST /ubus` handler implementing the 15 methods in §5.2. Most map
      trivially to NVS read/write, WiFi scan, reboot, and "upgrade" (which we
      can map to ESP OTA).
- [ ] `POST /cgi-bin/cgi-upload` multipart handler writing to a temp buffer,
      then invoking the ESP OTA writer.
- [ ] Session token management (`session.login` → NVS-stored password hash).
- [ ] First-boot flow: empty-password login → force password set.

### Out of scope for Phase 1

- [ ] TCP 5334 / Alexa bridge. The Alexa client itself is unreproducible
      (proprietary earsdb.com relay). Skip entirely; document as deprecated
      parity.
- [ ] SSH server. Use the ESP32 serial console for admin.
- [ ] Chinese localization bundles. Ship English only; add localization via
      standard i18n if/when needed.
- [ ] LuCI compatibility layer. Not referenced by the SPA.
- [ ] Riona Sans fonts. Replaced with Inter / system stack.

### Deliberate security improvements over Wi-DG

The Wi-DG ships with a long list of security holes that we will **not**
replicate. This is an intentional break from byte-for-byte parity — on the
specific axes listed below, our ESP32 reimplementation is tighter than the
original. A client that deliberately probes for these flaws will detect the
difference; Device Console, which only uses the UDP 3999 beacon and TCP 5333
raw pipe, will not.

| Axis                              | Wi-DG (stock)                                         | minidsp-open ESP32 (target)                                  |
|-----------------------------------|-------------------------------------------------------|---------------------------------------------------------------|
| SoftAP encryption                 | **None (open)**                                       | **WPA2-PSK** with printed-on-label password                  |
| Root SSH                          | **Enabled, empty password, 0.0.0.0:22**               | **Disabled entirely**                                         |
| WAN firewall input                | `input=ACCEPT` (all ports open on STA uplink)         | Match LAN policy; explicit rules for 5333, 80, 443           |
| TLS cert                          | Self-signed **RSA-1024**, `L=Uknown` typo             | Self-signed **ECDSA-P256**, `CN=miniDSP-open`                 |
| TLS cert validity                 | 730 days                                              | 3650 days (avoids nuisance re-gen)                            |
| `/www/log-minidsp.txt`            | Unauthenticated, public HTTP                          | Authenticated only (or removed)                               |
| `/alexa/status.json`              | Public HTTP — leaks auth token                        | N/A (Alexa skipped Phase 1)                                   |
| Port 8080 re-link trigger         | `0.0.0.0:8080`, no auth                               | N/A (Alexa skipped Phase 1)                                   |
| `Server:` HTTP header             | Omitted (no identification)                           | `Server: miniDSP-open` (explicit self-identification)         |
| DHCP client hostname              | Not advertised (empty option 12)                      | Advertises `miniDSP-open`                                     |
| USB fd synchronization            | No locking (race between TCP and USB threads)        | Proper mutex or single-consumer model                         |
| USB timeout retry                 | Tight loop, no yield, no bound                        | Yield + bounded retry                                         |
| Process supervision               | None — crash = dead until reboot                      | ESP task watchdog + restart                                   |
| Riona Sans fonts                  | Proprietary MediaTek typeface (license risk)          | Inter or system font stack                                    |

### Orphaned findings (noted for future work)

These are facts we've documented in detail docs but don't directly affect
Phase 1 parity:

- **I2C + WM8960 audio codec** is loaded at boot via `i2c_wm8960.ko` but
  I2S is disabled in the DTB. This is a dormant audio-output capability in
  the Wi-DG hardware that MiniDSP never wired up. `firmware-image.md` §6.
- **Hardware watchdog** exists in the DTB but no userspace daemon pets it.
  The Wi-DG has no watchdog-based reboot. `service-identity.md` §7.
- **`miniDSP Flex` device-name table entry** has a trailing space in the raw
  binary (entry 19, `hw_id=27, dsp_ver=100`). Strip it if we ever use the
  fallback path. `device-table.md` §2.
- **`/IoT/en.app.js`** is a stale 647 KB copy of the React SPA — 6,623 bytes
  smaller than the served `/www/build/en.app.js` and missing the `apsta`
  (dual-mode) keyword, meaning it pre-dates APSTA support. Dead code,
  referenced only by the `mtk-linkit.list` opkg manifest.
- **`/IoT/examples/`** contains three Arduino Yun-era GPIO blink demos
  (`blink-gpio44.js/.py`, `bus-test.js`). Dead code. Ignore.
- **17 stock uci-defaults files** run at first boot. Of those, exactly 3 are
  MiniDSP-authored (`51_linkit_config`, `52_linkit_firewall`, `54_linkit_version`).
  `53_linkit_samba` is stock LinkIt (sets up an SD-card Samba share that the
  Wi-DG hardware doesn't support — harmless vestige).

---

## 9. Provenance and completeness

- **Binary coverage.** All 21 user functions in `/IoT/minidsp` decompiled
  via Ghidra (driven directly over its Unix domain socket). All 29 global
  variables mapped to readers and writers. Zero hidden behaviors (no file
  I/O beyond stdio, no shell execution, no crypto, no signal handlers, no
  atexit callbacks, no additional HID opcodes beyond the three documented
  queries). The 0x14/0xD5 mystery is resolved: explicit SB immediates
  encoding `htons(5333)`. The second PLT at `0x00415000` is confirmed as
  empty GOT slot addresses, not executable code. See
  `app/widg-re/binary-audit.md`.
- **Rootfs coverage.** 5,641 files catalogued across all top-level dirs.
  Exactly 27 files contain the literal string "minidsp" / "Wi-DG" /
  "earsdb"; of those, `/IoT/init.sh` + `/IoT/minidsp` + `/IoT/alexa-client/`
  + `/IoT/en.app.js` (stale dead code) + 3 MiniDSP uci-defaults files
  (`51_linkit_config`, `52_linkit_firewall`, `54_linkit_version`) + 3 SPA
  language bundles + `linkit_rpc.json` ACL + `/etc/rc.local` launcher line
  are the complete set of MiniDSP-authored surface. Everything else matches
  stock OpenWrt 15.05.1 LinkIt Smart 7688 Duo base image signatures.
- **rpcd plugins.** `iwinfo.so` and `rpcsys.so` independently imported into
  Ghidra and decompiled function-by-function. Confirmed **byte-level stock**
  (unchanged vs upstream OpenWrt CC 15.05.1). No MiniDSP patches, no custom
  method handlers. See `app/widg-re/rpcd-plugins.md`.
- **Server fingerprint.** uhttpd (no `Server:` header), Dropbear 2017.75,
  TLS cert px5g-generated at first boot (`C=ZZ/ST=Somewhere/L=Uknown/CN=OpenWrt`,
  RSA-1024, 730 days), dnsmasq 12h lease, avahi service names containing
  the literal `LinkIt Smart7688` string, 4 non-default sysctl tweaks, no
  DHCP hostname option 12 — all captured verbatim in
  `app/widg-re/server-fingerprint.md`.
- **Alexa protocol.** 204-line app.js line-by-line walk. 9 WebSocket message
  types, 5 DSP commands with CRC-verified bytes, complete pairing flow
  against `wss://dev.earsdb.com/alexa`, security flaws catalogued. See
  `app/widg-re/alexa-protocol.md`.
- **Kernel.** Unmodified vanilla OpenWrt ramips/mt7688 kernel 3.18.91,
  GCC 4.8.3, built 2022-08-26. Embedded DTB confirms stock LinkIt Smart
  7688 board definition. No MiniDSP patches. See
  `app/widg-re/firmware-image.md`.
- **External observable behaviors (beacon, TCP framing, HID opcodes, web UI
  RPC surface, server fingerprints, mDNS, Alexa WS protocol)** are all
  specified to byte level in the companion docs.
- **Gap audit.** A final cross-document consistency pass resolved 7
  contradictions, confirmed 6 STATIC-resolvable items (all now answered),
  and bucketed the 16 LIVE-hardware items into the `live-verification-plan.md`
  priority list. The corpus is internally consistent as of 2026-04-10. See
  `app/widg-re/gap-audit.md`.
- **Residual unknowns** are enumerated in §7. All are LOW–MEDIUM confidence
  items about the *semantics* of confirmed-byte-value fields — the
  implementation emits the exact bytes the Wi-DG would, so any downstream
  client that accepts the Wi-DG will accept our ESP32 output regardless of
  how those fields are interpreted.

**Completeness statement (final).** To the best of the available evidence,
there is no externally observable Wi-DG behavior not documented by this
spec + its 22 companion docs. The static-RE knowledge budget is exhausted.
Any reimplementation that matches §3 and §5 byte-for-byte and produces the
boot timing described in §6 will be **indistinguishable from a real Wi-DG**
to every LAN client including stock MiniDSP Device Console, with the
explicit exceptions listed in §8 "Deliberate security improvements" where
we intentionally diverge to fix Wi-DG security holes.

**Nothing left to know that is answerable by reading files.** The remaining
7 live-verification tests are confirmations, not discoveries. The Phase 1
ESP32 implementation can proceed today using the static-RE values in this
spec; live testing when a physical Wi-DG is next on the bench will either
validate the implementation or highlight a small correction to a MEDIUM-confidence
byte-semantics call — none of which will require a structural redesign.
