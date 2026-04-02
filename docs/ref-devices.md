# minidsp-open: Device Registry

All data extracted from MiniDSP Device Console v2.10.6 (2026-03-29).
Sources:
- `app/MiniDSP Device Console.app/Contents/Resources/device_support_package/` (primary — SHARC devices)
- `app/apk-extracted/assets/www/js/controllers.js` (miniDSP Controller Android app — confirms source maps, adds legacy devices)
- minidsp-rs source code (legacy device definitions)

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
| 113 | Flex HT / Flex HTx (old FW, Dirac variant) |

Note: DSP 115 (Flex HT/HTx new FW with input PEQ) is NOT in the Dirac list — it replaces Dirac with input PEQ.

## Legacy Devices (Not in Device Console)

From miniDSP Controller app and minidsp-rs. These use ADAU1445 SigmaDSP (fixed-point, 2-byte addresses):

| hw_id | Device | Sources | Notes |
|-------|--------|---------|-------|
| 1 | 4x10 HD | SPDIF, TOSLINK, AES-EBU (or "Not Installed" if AN-FP module) | |
| 2 | miniDSP 2x4 (non-HD) / NanoDigi 2x8 | TOSLINK, SPDIF | Legacy VID 0x04D8 |
| 3 | PWR-ICE125/250 | LAN-only, see `FUTURE-pwr-ice.md` | dsp_version=51 |
| 4 | 10x10 HD (variant) | SPDIF, TOSLINK, AES-EBU | |
| 5 | 10x10 HD (variant) | SPDIF, TOSLINK, AES-EBU | |
| 7 | nanoAVR (variant) | Has HDMI source switching | |
| 8 | nanoAVR HDA | Has HDMI source switching | minidsp-rs #569, partially broken |
| 11 | NanoDigi 2x8 (variant) | TOSLINK, SPDIF | |

## Devices

### hw_id 10 — 2x4 HD / DDRC-24

| Field | 2x4 HD (DSP 100) | DDRC-24 (DSP 101) |
|-------|-------------------|-------------------|
| Name | 2x4 HD | DDRC-24 |
| DSP chip | ADI SHARC (ADSP21489) | Same |
| Controller | XMOS XU216 | Same |
| Sample rate | 96,000 Hz | 96,000 Hz |
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
| XMOS FW | v1.59 min, v1.85 latest | Same |
| DSP FW | v2.1 | v1.15 |
| Sources | 0:Analog, 1:Toslink, 2:USB | Same |
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
| XMOS FW | v1.59 min, v1.85 latest | Same |
| Sources | 0:Analog, 1:Toslink, 2:USB, 3:Bluetooth | Same |
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
| Sample rate | 96,000 Hz | 96,000 Hz |
| Inputs | 2 | 2 |
| Outputs | 8 | 8 |
| Input PEQ | 10 per channel | 10 per channel |
| Output PEQ | 10 per channel | 10 per channel |
| FIR | Yes (4096 max taps, 2048 per channel) | Yes |
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
| Dirac | No | No |
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
| Dirac | No | No |
| Presets | 4 | 4 |
| USB VID/PID | 0x2752/0x004B | Same |
| USB audio name (macOS) | miniDSP Flex HTx | Same |
| Dialect | New | New |
| XMOS FW | v1.77 min, v1.85 latest | Same |
| DSP FW | v1.14 (both) | Same |
| Sources | 0:Analog, 1:Toslink, 2:SPDIF, 3:USB, 4:HDMI | Same |
| Note | Has analog input (unlike Flex HT). | Input PEQ added in new FW — breaks minidsp-rs #701. |
| Has display | Yes | Yes |
| Has DRE option | Yes | Yes |
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
| Sources | 0:Toslink, 1:SPDIF, 2:AES-EBU, 3:RCA, 4:XLR, 5:USB, 6:LAN |
| minidsp-rs status | Supported |

---

### hw_id 17 — SHD Studio

| Field | Value |
|-------|-------|
| Name | SHD Studio |
| DSP ID | 105 (same as SHD) |
| Everything | Same as SHD except no built-in amplification |
| Sources | Same as SHD (7 sources) |
| USB VID/PID | 0x2752/0x0045 |
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
| Sources | **0:Toslink, 1:SPDIF, 2:AES-EBU, 3:USB, 4:LAN** |
| Note | Same DSP as SHD but **5 sources, not 7** (no RCA, no XLR — has built-in amp, no analog preamp inputs). |
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
hw_id 10 (2x4 HD):       0:Analog,  1:Toslink, 2:USB
hw_id 27 (Flex):          0:Analog,  1:Toslink, 2:USB,     3:Bluetooth
hw_id 30 (Flex Eight):    1:Toslink, 2:SPDIF,   3:USB,     4:Bluetooth     ← NO analog, starts at 1
hw_id 31 (Flex HT):       1:Toslink, 2:SPDIF,   3:USB,     4:HDMI          ← NO analog, starts at 1
hw_id 32 (Flex HTx):      0:Analog,  1:Toslink, 2:SPDIF,   3:USB, 4:HDMI
hw_id 14 (SHD):           0:Toslink, 1:SPDIF,   2:AES-EBU, 3:RCA, 4:XLR, 5:USB, 6:LAN
hw_id 17 (SHD Studio):    (same as SHD)
hw_id 21 (SHD Power):     0:Toslink, 1:SPDIF,   2:AES-EBU, 3:USB, 4:LAN   ← 5 sources (no RCA/XLR)
```

## DSP Structure Summary

| DSP ID | Used by | Freq | In/Out | Input PEQ | FIR | Key difference |
|--------|---------|------|--------|-----------|-----|----------------|
| 100 | 2x4 HD, Flex | 96kHz | 2/4 | Yes (10) | Yes (4096 taps) | Base structure |
| 101 | DDRC-24, Flex DL | 96kHz | 2/4 | Yes (10) | No | Dirac variant of 100 |
| 105 | SHD, SHD Studio, SHD Power | 96kHz | 2/4 | No | No | Dirac-only, no input PEQ |
| 110 | Flex Eight | 96kHz | 2/8 | Yes (10) | Yes (4096 taps) | 8 outputs |
| 111 | Flex Eight DL | 96kHz | 2/8 | Yes (10) | Yes | Dirac variant of 110 |
| 113 | Flex HT*, Flex HTx* | 48kHz | 8/8 | **No** | No | Old firmware, no input PEQ |
| 115 | Flex HT, Flex HTx | 48kHz | 8/8 | **Yes (10)** | No | New firmware, adds input PEQ + bass management |

## What's Needed to Generate Address Tables

The DSP_structure_layout.xml files define the signal chain (which modules, connections). The actual DSP register addresses are computed from these layouts. Two paths to get addresses:

1. **Dump from hardware** — connect device, run `minidsp probe` then `debug dump-float 0 255`. Map known parameter values to symbols. Best for unknown devices.
2. **Port the address computation** — the Device Console JS (`app.asar/build/src/templateDspDevice.js`, minified) computes addresses from structure layouts. Deobfuscate and port to our codegen.
3. **Reuse existing** — for devices sharing DSP structures with already-supported ones (SHD Power = SHD = DSP 105), the address table is identical. Only source mapping differs.

### Ready to add immediately (no hardware needed):
- **SHD Power** (hw_id 21) — same address table as SHD (DSP 105), just add hw_id + source mapping

### Need address dump from hardware OR deobfuscated address computation:
- **Flex Eight** (hw_id 30, DSP 110/111) — new structure, 8 outputs
- **Flex HT** (hw_id 31, DSP 113/115) — partially known from HTx, source mapping differs
- **Flex HTx new FW** (hw_id 32, DSP 115) — new structure with input PEQ

### Need device owner to probe:
- **CDSP 8x12DL** — zero data available
