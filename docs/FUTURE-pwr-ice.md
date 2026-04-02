# PWR-ICE125/250 Support (Future)

## BLOCKED: Need Help From a PWR-ICE Owner

Full PEQ/crossover support requires DSP register addresses that we cannot obtain ourselves. We have exhaustively searched the Device Console app, the Android miniDSP Controller APK, all public documentation, and every online source. **The addresses only exist in the standalone PWR_ICE2_2x2 plugin, which MiniDSP has effectively stopped distributing** (multiple owners report the UserDownloads page is empty as of 2024-2025).

**If you own a PWR-ICE, you can help by providing either:**

1. **The PWR_ICE2_2x2 plugin installer** (`.exe` or `.app`) — we can extract the register addresses from the binary using Ghidra or similar tools
2. **A Wireshark packet capture** of a full plugin session — connect via Ethernet, run Wireshark filtering TCP port 5333, then methodically change each PEQ band, crossover setting, gain, delay, and compressor parameter while noting what you changed. Save as `.pcapng`.
3. **A config.xml export** from the plugin (if the plugin supports exporting configuration — the older miniDSP plugins did)

Contact us on GitHub or post in the miniDSP community forum PWR-ICE section. User `m-diers` on GitHub (minidsp-rs issue #245) has a PWR-ICE and has already done basic packet captures.

**Without this, basic control works (volume, mute, preset switching) but PEQ, crossover, compressor, delay, FIR, and metering are not possible.**

---

Not shipping at launch. Different product line, different DSP chip, no Device Console XML, no address tables. Transport layer support (70-byte TCP frames) is included in minidsp-open core, but full device control (PEQ, crossover, delay, FIR, metering) requires the register addresses from the standalone plugin.

## Product Overview

The PWR-ICE125 ($399) and PWR-ICE250 ($499) are DSP-controlled plate amplifier modules for DIY active loudspeaker builders. ICEpower Class D amplification + MiniDSP DSP processing in one board.

- **PWR-ICE125:** ICEpower 125ASX2 -- 2x125W @ 4 ohm SE, 1x450W @ 4 ohm BTL
- **PWR-ICE250:** ICEpower 250ASX2 -- 2x250W @ 4 ohm SE, 1x500W @ 8 ohm BTL
- **DSP:** ADAU1445 SigmaDSP, 172MHz, 28/56-bit fixed-point, 96kHz
- **Topology:** 2 inputs / 2 outputs
- **Audio inputs:** Balanced XLR, unbalanced RCA, digital AES-EBU with ASRC
- **Connectivity:** LAN only (Ethernet RJ45). No USB. No WiFi.
- **Config software:** Standalone `PWR_ICE2_2x2` plugin (NOT Device Console)
- **Android/iOS app:** miniDSP Controller (com.dsp4you.minidsp) -- volume/mute/preset only, no PEQ

## Why It's Different From Everything Else

| Aspect | SHARC devices (2x4HD, Flex, HTx, SHD) | PWR-ICE |
|--------|---------------------------------------|---------|
| DSP chip | ADSP21489 SHARC (32-bit float) | ADAU1445 SigmaDSP (28/56-bit fixed-point) |
| Protocol dialect | New (3-byte addr, float32) or old (2-byte) | Old (2-byte addr, fixed-point) |
| Transport | USB HID (64-byte) + TCP (64-byte) | TCP only (70-byte frames) |
| Device Console support | Yes -- full XML definitions | None |
| hw_id | 10-32 (well documented) | 0 (unique, undocumented) |
| dsp_version | 100-115 (in Device Console XML) | 51 (not in any XML) |
| Address tables | Extracted from XML | Unknown -- must reverse-engineer |
| Frame padding | 0x00 | 0xFF |
| MAC OUI | varies | 00:50:C2 (DSP4YOU) |

## What Works Today (Generic Fallback)

With minidsp-open's auto-detected 70-byte TCP transport:

| Feature | Status |
|---------|--------|
| Discovery (UDP 3999) | Works -- broadcasts as "PWR-ICE2" |
| Master volume | Works |
| Master mute | Works |
| Preset switching (1-4) | Works |
| Source selection | Unknown -- reports "NotInstalled" |
| PEQ (input + output per channel) | Not working -- no address table |
| Crossover (IIR or FIR) | Not working -- no address table |
| Compressor/limiter | Not working -- no address table |
| Delay (up to 10/21.87ms) | Not working -- no address table |
| Level metering | Not working -- no address table |
| FIR loading | Not working -- no address table |

## Discovery Packet

72-byte UDP payload on port 3999:
```
Offset 0:    0x80 (magic)
Offset 2-3:  0x05 0xA0 (magic)
Offset 6-11: MAC address (00:50:C2 = DSP4YOU OUI)
Offset 14-17: IP address
Offset 18:   hw_id = 0
Offset 19:   fw_major = 3
Offset 20:   fw_minor = 0
Offset 21:   dsp_version = 51 (0x33)
Offset 22-23: serial number
Offset 36+:  hostname = "PWR-ICE2"
```

## TCP Frame Format

70-byte fixed-size frames on port 5333 (vs 64 for standard devices):
```
Byte 0:     Length (self-inclusive, same as standard protocol)
Bytes 1..N: Payload
Bytes N+1..63: Padding with 0xFF (not 0x00 like standard)
Bytes 64..69: 6 extra bytes (device-specific trailer, structure unknown)
```

Example response (HardwareId):
```
04 31 03 00 FF FF FF...FF [6 trailer bytes]
```
Length=4, payload=[0x31, 0x03, 0x00] = ReadHardwareId response, fw_major=3, fw_minor=0.

## Confirmed Protocol Commands

| Command | Bytes | Notes |
|---------|-------|-------|
| Mute on | `03 17 01 1B` | cmd=0x17, value=1 |
| Mute off | `03 17 00 1A` | cmd=0x17, value=0 |
| Preset 1 | `04 25 00 01 2A` | cmd=0x25, preset=0, reset=1 |
| Preset 2 | `04 25 01 01 2B` | cmd=0x25, preset=1, reset=1 |
| Preset 3 | `04 25 02 01 2C` | cmd=0x25, preset=2, reset=1 |
| Preset 4 | `04 25 03 01 2D` | cmd=0x25, preset=3, reset=1 |
| Volume | `03 42 XX checksum` | XX = abs(dB) x 2, range 0-255 |
| Read flash | `05 05 FF A3 30 DC` | addr=0xFFA3, size=48 (config block) |
| Read preset | `05 05 FF E0 01 EA` | addr=0xFFE0, size=1 (active preset) |

## Known Flash Addresses

| Address | Size | Purpose |
|---------|------|---------|
| 0xFFA1 | 1 | Current preset / config state |
| 0xFFA3 | 48 | Flash config block (serial, firmware) |
| 0xFFD0 | 48 | Device state block |
| 0xFFE0 | 1 | Active preset number |

## Existing Packet Captures

User m-diers shared pcap files: https://gist.github.com/m-diers/0b06f05ab6d71a3706be2158c39d63c6
These cover basic connect/status only -- not PEQ or crossover changes.

## DSP Architecture: What We Know From the Manual

From the PWR-ICE125/250 user manual (ManualsLib pages 27-39):

### Signal Processing Chain
```
Input -> Input PEQ -> Routing Matrix -> Output PEQ -> Crossover -> Compressor -> Delay -> Invert -> Gain -> Mute -> Output
```

### Per-Channel DSP Features
| Feature | Input Ch 1 | Input Ch 2 | Output Ch 1 | Output Ch 2 |
|---------|-----------|-----------|------------|------------|
| PEQ bands | unknown (probably 6-10) | same | unknown (probably 6) | same |
| Crossover | -- | -- | Yes (LP+HP, LR/BW/Bessel, up to 48dB/oct) | same |
| Compressor | -- | -- | Yes (threshold, ratio, attack, release) | same |
| Delay | -- | -- | Yes (up to 10ms/21.87ms depending on model) | same |
| Invert | -- | -- | Yes | same |
| Gain | Yes | Yes | Yes | same |
| Mute | Yes | Yes | Yes | same |

### Channel Modes
- **Stereo mode:** L input -> Ch1 output, R input -> Ch2 output
- **Mixed mono:** (L+R)/2 -> both outputs (for subwoofer use)
- **Bridged (BTL):** Single amplified output at higher power

### Sources
The PWR-ICE has analog (balanced XLR + unbalanced RCA) and digital (AES-EBU) inputs. Source selection in the plugin selects between analog and digital.

## Similar Devices in minidsp-rs (Same ADAU1445 Chip)

These devices use the same ADAU1445 SigmaDSP but with different SigmaStudio projects (different address layouts):

### miniDSP 2x4 (non-HD) -- hw_id 2, dsp_version 22
- **File:** `minidsp-rs/devtools/src/codegen/m2x4/config.xml`
- **Topology:** 2 inputs / 4 outputs
- **Dialect:** 2-byte addr, fixed-point
- **Address range:** 0-381 (382 symbols)
- **Key addresses from config.xml:**
  - GAIN_1940_ALG_NS1 = 10 (output 1 gain)
  - GAIN_1940_ALG_NS2 = 11 (output 2 gain)
  - EQ_CH_1_1_1_B0 = 12 (first biquad B0 coeff, input ch1 section 1 band 1)
  - MUTE_NO_SLEW_ALG_7MUTE = 5 (master mute)
  - MUTE_NO_SLEW_ALG_1MUTE = 7, _2MUTE = 6 (output mutes)
  - EQ1940_INVERT_1GAIN = 8 (output 1 phase invert)
  - MULT_CTRL_DEL_GROW_ALG_1 = 354 (output 1 delay)
  - EXT_SW_GAIN_DB_1STEP = 368 (master volume step)
  - MONO_ENVELOPE_PEAK_ALG_1HOLD/DECAY = 378/379 (level meters)
  - Each biquad = 5 consecutive addresses (B0, B1, B2, A1, A2)
  - PEQ groups: EQ_CH_{channel}_{section}_{band}_{coeff}

### NanoDigi 2x8 -- hw_id 2, dsp_version 54
- **File:** `minidsp-rs/devtools/src/codegen/nanodigi2x8/config.xml`
- **Topology:** 2 inputs / 8 outputs
- **Dialect:** 2-byte addr, fixed-point
- **Address range:** 0-914 (915 symbols)
- **Structure:** Same naming convention, just more channels

### Config XML Format
The config.xml files are exported from the miniDSP plugins. Format:
```xml
<setting version="1.13">
  <dspversion>22</dspversion>
  <item name="Gain1940AlgNS1" addr="10">
    <dec>0</dec>
    <hex>00800000</hex>
  </item>
  <filter name="EQ_ch1_1_1">
    <freq>10</freq>
    <q>0.5</q>
    <type>PK</type>
    ...
  </filter>
  <item name="EQ_ch1_1_1_B0" addr="12">
    <dec>1</dec>
    <hex>00800000</hex>
  </item>
  ...
</setting>
```
The `addr` attribute is the ADAU1445 parameter RAM address. The `name` is the SigmaStudio symbol name. These are exactly what we need for the PWR-ICE.

## Paths to Obtaining the Address Table (Ranked by Feasibility)

### Path 1: Decompile the PWR_ICE2_2x2 Plugin (BEST PATH)
**Feasibility: HIGH -- requires access to plugin installer**

The PWR_ICE2_2x2 plugin is an Adobe AIR application. It contains ActionScript/SWF code with embedded DSP register addresses as constants. Tools like JPEXS Free Flash Decompiler can extract the ActionScript source.

**Where to get the plugin:**
- MiniDSP UserDownloads page (requires account with PWR-ICE purchase): https://www.minidsp.com/userdownloads
- The plugin is NOT publicly downloadable -- it's gated behind purchase authentication
- Ask in the miniDSP community forum for someone to share the installer
- Contact miniDSP support with serial number to get download access
- There are TWO versions: `PWR_ICE2_2x2` (IIR only) and `PWR_ICE2_2x2_FIR` (IIR + FIR)

**What to look for after decompiling:**
- ActionScript constants mapping symbolic names to ADAU1445 addresses
- Config XML templates embedded in the app resources
- SigmaStudio export files (.params, .hex) in the app bundle

### Path 2: Extract config.xml From an Existing PWR-ICE Installation
**Feasibility: HIGH -- any PWR-ICE owner can do this in 5 minutes**

The plugin saves its configuration as XML files. A PWR-ICE owner just needs to:
1. Open the PWR_ICE2_2x2 plugin
2. Connect to their device (or use it offline)
3. Export/save the default configuration as XML
4. Share the XML file

The config.xml contains `<item name="..." addr="...">` entries that give us the complete address table. This is exactly how minidsp-rs got its address tables for the 2x4, nanodigi2x8, and other ADAU1445 devices.

**Where to find saved configs:**
- Windows: `%APPDATA%\miniDSP\PWR_ICE2_2x2\` or `%LOCALAPPDATA%\miniDSP\`
- macOS: `~/Library/Application Support/miniDSP/PWR_ICE2_2x2/`

### Path 3: Decompile the Android miniDSP Controller App
**Feasibility: MEDIUM -- app only does volume/mute/preset, unlikely to have PEQ addresses**

The miniDSP Controller app (com.dsp4you.minidsp, 6.5MB) is a Cordova/Angular app. APK contents would be at `assets/www/`. However, this app only supports basic remote control (volume, mute, preset, source). It does NOT support PEQ or crossover configuration.

**Assessment:** The app is unlikely to contain DSP register addresses for PEQ/crossover since it doesn't implement those features. It may contain useful protocol details (discovery, command format) but not the address table we need. The app does support PWR-ICE for volume control (firmware v3.2+).

**APK availability:** Available on APKPure, Aptoide, APKCombo, but all sites require browser JavaScript execution for download. Direct curl does not work due to Cloudflare/bot protection. Package name: `com.dsp4you.minidsp`, latest version 1.0.11 (6.5MB).

### Path 4: Full Protocol Capture With a Real Device
**Feasibility: MEDIUM -- requires a PWR-ICE owner willing to help**

A PWR-ICE owner captures a full Wireshark session of the plugin connecting and changing every parameter:
1. Start Wireshark, filter for TCP port 5333
2. Open PWR_ICE2_2x2 plugin, connect to device
3. Systematically change every PEQ band, crossover, delay, gain, compressor setting
4. Note which parameter was changed for each captured packet
5. Save as .pcapng

This lets us correlate plugin UI actions to protocol writes, extracting the address table from the wire.

### Path 5: SigmaStudio Project File
**Feasibility: LOW -- MiniDSP is unlikely to share this**

If the ADAU1445 SigmaStudio project file (.dspproj) for the PWR-ICE were available, all parameter RAM addresses would be directly readable. However, this is MiniDSP's proprietary firmware design and they are very unlikely to share it.

### Path 6: Brute-Force Register Scan
**Feasibility: LOW -- tedious but possible with hardware**

With a PWR-ICE connected:
```
minidsp --tcp <device-ip> debug dump-float 0 4095
```
Read all 4096 parameter RAM addresses and map them by:
- Comparing values across presets
- Changing a single PEQ and reading back all addresses to find which changed
- The ADAU1445 has 4K parameter RAM (addresses 0x0000-0x0FFF)

This is very tedious but doesn't require the plugin software at all.

## Related GitHub Issues and PRs

| Reference | Description |
|-----------|-------------|
| [minidsp-rs #245](https://github.com/mrene/minidsp-rs/issues/245) | PWR-ICE support request. Contains discovery packet dumps, HardwareId capture, and user discussion. Key finding: hw_id=3 (from `[31, 03, 00]` response), 70-byte TCP frames. User m-diers provided 3 pcap files. |
| [minidsp-rs #313](https://github.com/mrene/minidsp-rs/pull/313) | PR fixing 70-byte frame transport for PWR-ICE. Merged to dev. |
| [minidsp-rs probe.rs](https://github.com/mrene/minidsp-rs/blob/dev/protocol/src/device/probe.rs) | Device probing code. PWR-ICE NOT in the match table (hw_id=3 not listed). Falls through to Generic. |

## Key Technical Details From Research

1. **hw_id is actually 3, not 0.** The discovery packet showed hwid=0 but the HardwareId TCP response `[31, 03, 00]` shows hw_id=3, fw_version=0. The discovery packet's hwid field is a different encoding.

2. **The miniDSP 2x4 (non-HD) and PWR-ICE both use ADAU1445**, but with DIFFERENT SigmaStudio projects. The 2x4 is a 2-in/4-out topology, while PWR-ICE is 2-in/2-out. They share the same DSP architecture but have different register layouts.

3. **minidsp-rs uses config.xml for all ADAU1445 devices.** The pattern in devtools is: take a config.xml export from the plugin, parse it to extract the symbol-to-address mapping, then generate a Rust device definition. This is the same approach we should use.

4. **Two plugin variants exist:**
   - `PWR_ICE2_2x2` -- IIR-only version
   - `PWR_ICE2_2x2_FIR` -- IIR + FIR version (preferred, has FIR filter loading)

5. **Plugin requires Adobe AIR runtime** (Windows/macOS). It is NOT an Electron app. Installation requires .NET Framework, Adobe Flash, and Adobe AIR (all legacy technologies).

6. **Plugin has a "Remote Mode"** (v1.4+) that enables the mobile app to control volume. This suggests the plugin may have a secondary TCP protocol for remote control that differs from the DSP programming protocol.

7. **Forum posts indicate the plugin is hard to get.** Multiple users report difficulty downloading it from UserDownloads. MiniDSP gates it behind purchase authentication. Second-hand buyers need to contact support with serial numbers.

## Implementation Plan (When Ready)

1. Obtain config.xml from a PWR-ICE owner (Path 2) or decompile the plugin (Path 1)
2. Parse the config.xml to extract symbol-to-address mapping
3. Create `devtools/src/codegen/pwr_ice/config.xml` and `mod.rs`
4. Add hw_id 3 / dsp_version 51 to the probe table
5. Generate the device definition with `minidsp-devtools codegen`
6. Implement fixed-point value encoding/decoding (shared with m2x4/nanodigi2x8)
7. Handle 70-byte TCP frame transport (already done in core)
8. Test with a real device

Estimated effort: 1-2 days once config.xml is obtained. The codegen pipeline already handles ADAU1445 devices.

## Action Items

- [ ] Post in miniDSP community forum (https://www.minidsp.com/community/forums/pwr-ice.24/) asking for:
  1. A saved config.xml from the PWR_ICE2_2x2 plugin
  2. Or the plugin installer itself for decompilation
- [ ] Contact m-diers (GitHub user from minidsp-rs #245) who owns a PWR-ICE and has already been capturing packets
- [ ] If we get the plugin, decompile with JPEXS Free Flash Decompiler to extract ActionScript constants
- [ ] If we get config.xml, generate device definition using the existing minidsp-rs codegen pipeline pattern
