# minidsp-open: Device Registry

All data extracted from MiniDSP Device Console v2.10.6 (2026-03-29).
Sources:
- `app/MiniDSP Device Console.app/Contents/Resources/device_support_package/` (primary — SHARC devices)
- `app/apk-extracted/assets/www/js/controllers.js` (miniDSP Controller Android app — confirms source maps, adds legacy devices)
- minidsp-rs source code (legacy device definitions)

This file is a registry and planning reference, not a blanket claim that every listed device is already fully implemented and hardware-verified in `minidsp-open`.

## Coverage Policy

We do not have physical access to the full MiniDSP product line. Full device-family coverage therefore has to be driven by a repeatable import/enumeration workflow using the official app assets:

1. extract authoritative device metadata from `app/app-extracted` and `app/apk-extracted`
2. map each official-app device to probe identity, sources, capabilities, and config typing
3. compare that inventory against the current Rust support table
4. implement missing definitions systematically, using the 2x4 HD as the live regression case for shared protocol behavior

Verification tiers used in practice:
- `Hardware verified`: behavior confirmed on the physical 2x4 HD we have
- `Data modeled`: device definition/capabilities imported from official app assets, but not physically tested here
- `Basic support`: probe/master-status style support only

Current reality:
- 2x4 HD is the only device with direct live hardware validation in this workspace
- broader support claims must stay tied to extracted official-app evidence until more hardware exists

## Dirac-Capable DSP Versions

From miniDSP Controller app: `[61, 94, 95, 101, 105, 111, 113]`

| dsp_version | Device |
|-------------|--------|
| 61 | Legacy (unknown, pre-HD) |
| 94 | Legacy (unknown, pre-HD) |
| 95 | Legacy (unknown, pre-HD) |
| 101 | DDRC-24 |
| 105 | SHD / SHD Studio / SHD Power |
| 111 | Flex Eight DL |
| 113 | Flex HT / Flex HTx (old FW) |

Note: DSP 115 (Flex HT/HTx new FW with input PEQ) is NOT in the Dirac list — the firmware replaced Dirac with input PEQ. The Device Console XML still contains Dirac address blocks for DSP 115 (layout artifact from the shared structure), but the firmware does not activate them.

Dirac capability here means the firmware has the code path and valid DSP addresses. Whether Dirac is actually active depends on product licensing — not all products shipping with a Dirac-capable DSP version have Dirac enabled.

## Legacy Devices (Not in Device Console)

From miniDSP Controller app and minidsp-rs. These use ADAU1445 SigmaDSP (fixed-point, 2-byte addresses):

| hw_id | Device | Sources | Notes |
|-------|--------|---------|-------|
| 1 | 4x10 HD | SPDIF, TOSLINK, AES-EBU (or "Not Installed" if AN-FP module) | |
| 2 | miniDSP 2x4 (non-HD) / NanoDigi 2x8 | TOSLINK, SPDIF | Legacy VID 0x04D8 |
| 3 | PWR-ICE125/250 | LAN-only, see `FUTURE-pwr-ice.md` | dsp_version=51 |
| 4 | miniSHARC 4x8 / 10x10 HD (variant) | SPDIF, TOSLINK, AES-EBU | APK groups hw_id 4 with 1 and 5 for sources; minidsp-rs maps `(4, _) -> MSharc4x8`. Likely shared hw_id. |
| 5 | 10x10 HD (variant) | SPDIF, TOSLINK, AES-EBU | |
| 6 | DDRC-88BM | TOSLINK, SPDIF | dsp_version=95, new dialect (SHARC). Not in APK or Device Console. minidsp-rs maps `(6, 95) -> DDRC88BM`. |
| 7 | nanoAVR (variant) | Has HDMI source switching (cmd 0x49) | |
| 8 | nanoAVR HDA | Has HDMI source switching (cmd 0x49) | minidsp-rs #569, partially broken |
| 18 | SHD Studio (variant) | TOSLINK, SPDIF, AES-EBU, USB, LAN | APK groups with hw_id 17. Not in Device Console. Likely older or regional SHD Studio SKU. |
| 11 | C-DSP 8x12 v2 | TOSLINK, SPDIF | dsp_version=97, new dialect (SHARC despite legacy hw_id). minidsp-rs maps `(11, 97) -> C8x12v2`. APK groups with hw_id 2 for sources. |

## Devices

### hw_id 10 — 2x4 HD / DDRC-24

| Field | 2x4 HD (DSP 100) | DDRC-24 (DSP 101) |
|-------|-------------------|-------------------|
| Name | 2x4 HD | DDRC-24 |
| DSP chip | ADI SHARC (ADSP21489) | Same |
| Controller | XMOS XU216 | Same |
| Sample rate | 96,000 Hz | 48,000 Hz (Dirac requires lower rate) |
| Inputs | 2 | 2 |
| Outputs | 4 | 4 |
| Input PEQ | 10 per channel | 10 per channel |
| Output PEQ | 10 per channel | 10 per channel |
| FIR | Yes (4096 max taps) | No |
| Compressor | Yes (per output) | Yes (per output) |
| Crossover | Yes (per output) | Yes (per output) |
| Dirac | No | Yes |
| Presets | 4 | 4 |
| USB VID/PID | 0x2752/0x0011 | 0x2752/0x0044 |
| USB audio name (macOS) | miniDSP 2x4HD | DDRC-24 |
| Dialect | New (3-byte addr, float32) | New |
| XMOS FW | v1.50 min, v1.85 latest | Same |
| DSP FW | v2.0 | v2.0 |
| Sources | 0:Analog, 1:Toslink, 2:USB | Same |
| REW supported | Yes | Yes |
| minidsp-rs status | Supported | Supported |

---

### hw_id 27 — Flex / Flex DL

| Field | Flex (DSP 100) | Flex DL (DSP 101) |
|-------|----------------|-------------------|
| Name | Flex | Flex (DL) |
| DSP chip | ADI SHARC | Same |
| Controller | XMOS | Same |
| Sample rate | 96,000 Hz | 48,000 Hz (with Dirac) |
| Inputs | 2 | 2 |
| Outputs | 4 | 4 |
| Input PEQ | 10 per channel | 10 per channel |
| Output PEQ | 10 per channel | 10 per channel |
| FIR | Yes (4096 max taps) | No |
| Compressor | Yes | Yes |
| Crossover | Yes | Yes |
| Dirac | No | Yes |
| Presets | 4 | 4 |
| USB VID/PID | 0x2752/0x0011 | 0x2752/0x0044 |
| USB audio name (macOS) | miniDSP Flex | miniDSP Flex DL |
| Dialect | New | New |
| XMOS FW | v1.76 min, v1.85 latest | Same |
| DSP FW | v2.0 | v2.0 |
| Sources | 0:Analog, 1:Toslink, 2:SPDIF, 3:USB, 4:Bluetooth | Same |
| Has Bluetooth module | Yes | Yes |
| Has display | Yes (OLED) | Yes |
| REW supported | Yes | Yes |
| minidsp-rs status | Supported | Supported |

---

### hw_id 30 — Flex Eight / Flex Eight DL *(NEW — missing in minidsp-rs)*

| Field | Flex Eight (DSP 110) | Flex Eight DL (DSP 111) |
|-------|---------------------|------------------------|
| Name | Flex Eight | Flex Eight (DL) |
| DSP chip | ADI SHARC | Same |
| Controller | XMOS | Same |
| Sample rate | 96,000 Hz | 48,000 Hz (Dirac requires lower rate) |
| Inputs | 2 | 2 |
| Outputs | 8 | 8 |
| Input PEQ | 10 per channel | 10 per channel |
| Output PEQ | 10 per channel | 10 per channel |
| FIR | Yes (4096 max taps, 2048 per channel) | No (Dirac replaces FIR) |
| Compressor | Yes (per output) | Yes |
| Crossover | Yes (LPF8 + HPF8 per output) | Yes |
| Dirac | No | Yes |
| Presets | 4 | 4 |
| USB VID/PID | 0x2752/0x0012 | 0x2752/0x004A |
| USB audio name (macOS) | miniDSP Flex 8 | miniDSP Flex 8 DL |
| Dialect | New (3-byte addr, float32) | New |
| XMOS FW | v1.76 min, v1.85 latest | Same |
| DSP FW | v2.1 (non-DL), v1.15 (DL) | v1.15 |
| Sources | 1:Toslink, 2:SPDIF, 3:USB, 4:Bluetooth | Same |
| Note | **No analog input.** Source idx starts at 1, not 0. | Same |
| Has Bluetooth module | Yes | Yes |
| Has display | Yes | Yes |
| Has DRE option | Yes | Yes |
| REW supported | Yes | Yes |
| minidsp-rs status | **MISSING** — picks up as Generic, source/PEQ broken |

---

### hw_id 31 — Flex HT *(NEW — missing in minidsp-rs)*

| Field | Flex HT (DSP 113, old FW) | Flex HT (DSP 115, new FW) |
|-------|--------------------------|--------------------------|
| Name | Flex HT* | Flex HT |
| DSP chip | ADI SHARC | Same |
| Controller | XMOS | Same |
| Sample rate | 48,000 Hz | 48,000 Hz |
| Inputs | 8 | 8 |
| Outputs | 8 | 8 |
| Input PEQ | **None** (old firmware) | **10 per channel** (new firmware) |
| Output PEQ | 10 per channel | 10 per channel |
| FIR | No | No |
| Compressor | Yes | Yes |
| Crossover | Yes (includes bass management HPF/LPF) | Yes |
| Dirac | Yes (firmware capable) | No (replaced by input PEQ) |
| Presets | 4 | 4 |
| USB VID/PID | 0x2752/0x004B | Same |
| USB audio name (macOS) | miniDSP Flex HT | Same |
| Dialect | New | New |
| XMOS FW | v1.77 min, v1.85 latest | Same |
| DSP FW | v1.14 (both) | Same |
| Sources | 1:Toslink, 2:SPDIF, 3:USB, 4:HDMI | Same |
| Note | **No analog input.** Shares DSP structure with HTx. | Adds input PEQ — different address layout than DSP 113. |
| Has display | Yes | Yes |
| Has DRE option | Yes | Yes |
| REW supported | Yes | Yes |
| minidsp-rs status | **MISSING** — `--force-kind flexhtx` partially works, source broken |

---

### hw_id 32 — Flex HTx

| Field | Flex HTx (DSP 113, old FW) | Flex HTx (DSP 115, new FW) |
|-------|---------------------------|---------------------------|
| Name | Flex HTx* | Flex HTx |
| DSP chip | ADI SHARC | Same |
| Controller | XMOS | Same |
| Sample rate | 48,000 Hz | 48,000 Hz |
| Inputs | 8 | 8 |
| Outputs | 8 | 8 |
| Input PEQ | **None** (old firmware) | **10 per channel** (new firmware) |
| Output PEQ | 10 per channel | 10 per channel |
| FIR | No | No |
| Compressor | Yes | Yes |
| Crossover | Yes (includes bass management) | Yes |
| Dirac | Yes (firmware capable) | No (replaced by input PEQ) |
| Presets | 4 | 4 |
| USB VID/PID | 0x2752/0x004B | Same |
| USB audio name (macOS) | miniDSP Flex HTx | Same |
| Dialect | New | New |
| XMOS FW | v1.77 min, v1.85 latest | Same |
| DSP FW | v1.14 (both) | Same |
| Sources | 0:Analog, 1:Toslink, 2:SPDIF, 3:USB, 4:HDMI | Same |
| Note | Has analog input (unlike Flex HT). | Input PEQ added in new FW — breaks minidsp-rs #701. |
| Has display | Yes | Yes |
| Has DRE option | **No** (not in Device Console XML — only Flex Eight and Flex HT have DRE) | **No** |
| REW supported | Yes | Yes |
| minidsp-rs status | Supported (DSP 113 only) — **DSP 115 BROKEN** |

---

### hw_id 14 — SHD

| Field | Value |
|-------|-------|
| Name | SHD |
| DSP ID | 105 |
| DSP chip | ADI SHARC |
| Controller | XMOS |
| Sample rate | 96,000 Hz |
| Inputs | 2 |
| Outputs | 4 |
| Input PEQ | None (Dirac handles input processing) |
| Output PEQ | 10 per channel |
| Compressor | Yes |
| Crossover | Yes |
| Dirac | Yes (always) |
| Presets | 4 |
| USB VID/PID | 0x2752/0x0045 |
| Dialect | New |
| XMOS FW | v1.76 min, v1.85 latest |
| DSP FW | v1.15 |
| Sources | 0:Toslink, 1:SPDIF, 2:AES-EBU, 3:Analog Single-End, 4:Analog Balanced, 5:USB, 6:LAN |
| Source name note | XML uses "Analog Single-End" (idx 3) and "Analog Balanced" (idx 4). APK and minidsp-rs use "RCA" and "XLR". Both refer to the same physical connectors. |
| Has display | Yes (DIM_CTRL: idle 3-120s, brightness 1-8) |
| REW supported | Yes |
| minidsp-rs status | Supported |

---

### hw_id 17 — SHD Studio

| Field | Value |
|-------|-------|
| Name | SHD Studio |
| DSP ID | 105 (same as SHD) |
| Everything | Same as SHD except: no built-in amplification, no analog inputs |
| Sources | **0:Toslink, 1:SPDIF, 2:AES-EBU, 3:USB, 4:LAN** (5 sources — no analog inputs unlike SHD) |
| USB VID/PID | 0x2752/0x0045 |
| XMOS FW | v1.76 min, v1.85 latest |
| DSP FW | v1.15 |
| Has display | Yes (same DIM_CTRL as SHD) |
| REW supported | Yes |
| hw_id 18 note | APK groups `hw_id == 17 \|\| hw_id == 18` with same sources. hw_id 18 is not in Device Console — likely older or regional variant. |
| minidsp-rs status | Supported (shares SHD definition) |

---

### hw_id 21 — SHD Power *(NEW — missing in minidsp-rs)*

| Field | Value |
|-------|-------|
| Name | SHD Power |
| DSP ID | 105 (same as SHD) |
| DSP chip | ADI SHARC |
| Controller | XMOS |
| Sample rate | 96,000 Hz |
| Inputs | 2 |
| Outputs | 4 |
| Output PEQ | 10 per channel |
| Compressor | Yes |
| Crossover | Yes |
| Dirac | Yes |
| Presets | 4 |
| USB VID/PID | 0x2752/0x0045 |
| Dialect | New |
| XMOS FW | v1.76 min, v1.85 latest |
| DSP FW | v1.15 |
| Sources | **0:Toslink, 1:SPDIF, 2:AES-EBU, 3:USB, 4:LAN** |
| Note | Same DSP as SHD but **5 sources, not 7** (no RCA, no XLR — has built-in amp, no analog preamp inputs). |
| Has display | Yes (same DIM_CTRL as SHD) |
| REW supported | Yes |
| minidsp-rs status | **MISSING** — add hw_id 21 to SHD match + add 5-source mapping. **Trivial fix.** |

---

## USB VID/PID Registry

| VID | PID | Device(s) |
|-----|-----|-----------|
| 0x04D8 | 0x003F | Legacy PIC-based products (2x4 non-HD, etc.) |
| 0x2752 | 0x0011 | 2x4 HD, Flex (non-DL) |
| 0x2752 | 0x0012 | Flex Eight (non-DL) |
| 0x2752 | 0x0013 | HA-DSP |
| 0x2752 | 0x0040 | DDRC-22 |
| 0x2752 | 0x0043 | DDRC-88 |
| 0x2752 | 0x0044 | DDRC-24, Flex DL |
| 0x2752 | 0x0045 | SHD, SHD Studio, SHD Power |
| 0x2752 | 0x0047 | CDSP-8x12 / CDSP-8x12DL |
| 0x2752 | 0x0048 | Harmony DSP |
| 0x2752 | 0x004A | Flex Eight DL |
| 0x2752 | 0x004B | Flex HT, Flex HTx |

## Source Mappings (idx → name, per hw_id)

```
hw_id 10 (2x4 HD):       0:Analog,  1:Toslink, 2:USB                                                   [no voloffsets]
hw_id 27 (Flex):          0:Analog,  1:Toslink, 2:SPDIF,   3:USB, 4:Bluetooth                           [voloffsets]
hw_id 30 (Flex Eight):    1:Toslink, 2:SPDIF,   3:USB,     4:Bluetooth     ← NO analog, starts at 1     [voloffsets]
hw_id 31 (Flex HT):       1:Toslink, 2:SPDIF,   3:USB,     4:HDMI          ← NO analog, starts at 1     [voloffsets]
hw_id 32 (Flex HTx):      0:Analog,  1:Toslink, 2:SPDIF,   3:USB, 4:HDMI                                [voloffsets]
hw_id 14 (SHD):           0:Toslink, 1:SPDIF,   2:AES-EBU, 3:Analog S-E, 4:Analog Bal, 5:USB, 6:LAN    [voloffsets]
hw_id 17 (SHD Studio):    0:Toslink, 1:SPDIF,   2:AES-EBU, 3:USB, 4:LAN   ← 5 sources (no analog)      [voloffsets]
hw_id 21 (SHD Power):     0:Toslink, 1:SPDIF,   2:AES-EBU, 3:USB, 4:LAN   ← 5 sources (no analog)      [voloffsets]
```

Note: `voloffsets` = Device Console XML `voloffsets="true"` on `<AUDIO_IN_SOURCES>`. hw_id 10 (2x4 HD) is the only SHARC device without per-source volume offsets. SHD source names: XML uses "Analog Single-End"/"Analog Balanced"; APK and minidsp-rs use "RCA"/"XLR".

## DSP Structure Summary

| DSP ID | Used by | Freq | In/Out | Input PEQ | FIR | Key difference |
|--------|---------|------|--------|-----------|-----|----------------|
| 100 | 2x4 HD, Flex | 96kHz | 2/4 | Yes (10) | Yes (4096 taps) | Base structure |
| 101 | DDRC-24, Flex DL | 48kHz | 2/4 | Yes (10) | No | Dirac variant of 100 |
| 105 | SHD, SHD Studio, SHD Power | 96kHz | 2/4 | No | No | Dirac-only, no input PEQ |
| 110 | Flex Eight | 96kHz | 2/8 | Yes (10) | Yes (4096 taps) | 8 outputs |
| 111 | Flex Eight DL | 48kHz | 2/8 | Yes (10) | No | Dirac variant of 110 |
| 113 | Flex HT*, Flex HTx* | 48kHz | 8/8 | **No** | No | Old firmware, Dirac capable, no input PEQ |
| 115 | Flex HT, Flex HTx | 48kHz | 8/8 | **Yes (10)** | No | New firmware, adds input PEQ + bass management, no Dirac |

## Address Table Status

Canonical source:
- `docs/ref-address-tables.md` — SHARC address tables
- `docs/ref-legacy-devices.md` — legacy address sourcing and deferred devices

This file is the device registry only: hw_ids, dsp_versions, source maps, USB VID/PID, and capability notes.

Current status:
- SHARC address tables needed for launch are already documented in `docs/ref-address-tables.md`
- SHD Power reuses DSP 105 with a different source map
- CDSP 8x12DL remains deferred pending a device owner probe
