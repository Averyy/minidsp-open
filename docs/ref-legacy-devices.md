# minidsp-open: Legacy Device Address Tables & Analysis

Research date: 2026-03-30. All data from minidsp-rs `dev` branch source code, MiniDSP Device Console, and miniDSP Controller Android APK.

Important status note:
- this document shows what legacy-device source data exists and how to turn it into support
- it does not mean legacy full support is already complete
- the required next step is a repeatable clean-room importer from the extracted official app assets plus legacy `config.xml` sources

## Executive Summary

Complete address tables for every legacy device exist in SigmaStudio config.xml exports (the same files minidsp-rs uses as source data). These config.xml files are MiniDSP device data (SigmaStudio project exports), not minidsp-rs code. We use them as source data for our own device definitions, same as we use Device Console XML for SHARC devices.

**Status:** Source data exists for full legacy support, but `minidsp-open` currently ships legacy devices at `DspKnown` or basic-support tiers until the clean-room importer and `config.xml` extraction path are implemented. Treat this file as source-data and planning reference, not as a claim that full legacy parity is already shipped.

**Clean-room note:** We reference minidsp-rs's protocol knowledge (credited) and use the same public source data (SigmaStudio config.xml exports). All code — extraction, device definitions, dialect handling — is written from scratch. We do NOT copy minidsp-rs's generated Rust files or their codegen pipeline.

## How minidsp-rs Handles Legacy Devices

### Architecture (dev branch)

minidsp-rs `dev` branch (the active development branch, protected) has a clean multi-crate architecture:

```
minidsp-rs/dev/
  protocol/src/
    dialect.rs          -- AddrLen2/AddrLen3, FixedPoint/Float32LE encoding
    fixed_point.rs      -- 5.23 fixed-point codec (ADAU1445 native format)
    commands.rs         -- Addr struct with variable-length encoding, Value with FixedPoint variant
    device/
      mod.rs            -- Device struct with `dialect: Dialect` field
      probe.rs          -- hw_id + dsp_version -> DeviceKind mapping
      m2x4.rs           -- Generated: miniDSP 2x4 (non-HD), 382 symbols, ~34KB
      m4x10hd.rs        -- Generated: 4x10 HD, ~38KB
      m10x10hd.rs       -- Generated: 10x10 HD, ~63KB
      nanodigi2x8.rs    -- Generated: NanoDigi 2x8, ~33KB
      msharc4x8.rs      -- Generated: miniSHARC 4x8, ~44KB
      ddrc88bm.rs       -- Generated: DDRC-88BM (bass management), ~62KB
      c8x12v2.rs        -- Generated: C-DSP 8x12 v2, ~79KB
      m2x4hd.rs         -- Generated: 2x4 HD (SHARC, new dialect)
      flex.rs           -- Generated: Flex
      flexdl.rs         -- Generated: Flex DL
      flexhtx.rs        -- Generated: Flex HTx
      shd.rs            -- Generated: SHD
      ddrc24.rs         -- Generated: DDRC-24
  devtools/src/codegen/
    mod.rs              -- XML config.xml parser + Rust code generator
    spec.rs             -- Intermediate spec structs (symbolic names, not addresses)
    spec_to_tokens.rs   -- Converts spec to Rust token stream
    m2x4/config.xml     -- 51KB, SigmaStudio XML export
    m4x10hd/config.xml  -- 42KB
    m10x10hd/config.xml -- 76KB
    nanodigi2x8/config.xml -- 46KB
    msharc4x8/config.xml   -- 239KB
    ddrc88bm/config.xml    -- 80KB
    c8x12v2/config.xml    -- 115KB
    (+ SHARC devices)
```

### Codegen Pipeline

1. Each device has a `config.xml` -- a SigmaStudio project export containing all DSP block names and their memory addresses
2. `devtools/src/codegen/{device}/mod.rs` defines the device's signal chain structure (which symbols map to which logical parameters)
3. The codegen parses config.xml to build a symbol->address bimap, then resolves symbols to produce a static `DEVICE` constant with numeric addresses
4. Generated files are checked into `protocol/src/device/{device}.rs`

### Dialect System

The `Dialect` struct in `protocol/src/dialect.rs` handles all old-vs-new encoding differences in one place:

| Aspect | Old dialect (AddrLen2 + FixedPoint) | New dialect (AddrLen3 + Float32LE) |
|--------|-------------------------------------|-------------------------------------|
| Address encoding | 2 bytes, `0x80` prefix for write bit | 3 bytes, `0x80` prefix for write bit |
| Float values | 5.23 fixed-point (u32 BE) | IEEE float32 LE |
| Mute on/off | `0x00000000` / `0x00800000` | `1` / `2` (u16) |
| Invert on/off | `0xFF800000` / `0x00800000` | `1` / `0` (u16) |
| Delay | Int32 (sample count, BE) | Int16 (sample count) |
| Biquad coefficients | 5x fixed-point (u32 BE each) | 5x float32 LE |

This is already fully implemented in minidsp-rs `dev` and works correctly for all legacy devices.

## Legacy Device Inventory

### Tier 1: Full address tables exist in source data (follow-on implementation work)

| Device | hw_id | dsp_version | Dialect | I/O | PEQ per ch | Config.xml | Generated .rs |
|--------|-------|-------------|---------|-----|------------|------------|---------------|
| **4x10 HD** | 1 | other | Old | 4in/10out | 5 in, 5 out | 42KB | 38KB |
| **10x10 HD** | 1 | 51 | Old | 10in/10out | 6 in, 5 out | 76KB | 63KB |
| **miniDSP 2x4** | 2 | 22 | Old | 0in/4out | 0 in, 6 out* | 51KB | 34KB |
| **NanoDigi 2x8** | 2 | 54 | Old | 2in/8out | 5 in, 5 out | 46KB | 33KB |
| **miniSHARC 4x8** | 4 | any | New** | 4in/8out | 10 in, 10 out | 239KB | 44KB |
| **DDRC-88BM** | 6 | 95 | New** | 8in/8out | 0 in, 10 out | 80KB | 62KB |
| **C-DSP 8x12 v2** | 11 | 97 | New** | 8in/12out | varies | 115KB | 79KB |

*miniDSP 2x4: minidsp-rs has symbols extracted but the `inputs` and `outputs` arrays are empty in the codegen mod.rs. The symbols ARE there (PEQ, mute, gain, delay, invert, crossover addresses all present). The codegen just never wired them up. This means the 2x4 currently only gets basic support (volume/mute/source/preset). Full PEQ support would require writing the input/output mapping in the codegen mod.rs using the existing symbols.

**miniSHARC 4x8, DDRC-88BM, C-DSP 8x12 v2: These use the DEFAULT dialect (3-byte addr, float32) despite being listed as "legacy" in some docs. They use ADI SHARC processors, not ADAU1445.

### Tier 2: Basic support only (no full address tables)

| Device | hw_id | dsp_version | Notes |
|--------|-------|-------------|-------|
| **nanoAVR** | 7 | ? | HDMI switching. Not in minidsp-rs codegen. No config.xml. Basic support only. |
| **nanoAVR HDA** | 8 | ? | HDMI switching. minidsp-rs issue #569 reports partial/broken support. No config.xml. |
| **4x10 HD variant** | 4 | ? | Some hw_id 4 devices are miniSHARC, some are 4x10HD. Unclear mapping. |
| **10x10 HD variant** | 5 | ? | Alternate hw_id for 10x10HD. May share address table with hw_id 1/dsp 51. |

### Tier 3: Deferred (already decided)

| Device | hw_id | Status |
|--------|-------|--------|
| **PWR-ICE125/250** | 3 (dsp 51) | Deferred per `FUTURE-pwr-ice.md`. LAN-only, no config.xml, no address tables. |

## Legacy vs SHARC: Complexity Comparison

### What's simpler about legacy devices:
- **Smaller address space:** 2x4 has 382 symbols (max addr 381). 4x10HD has ~915 symbols. Compare to 2x4HD's 20,000+ addresses.
- **Fewer features:** No FIR filters (except miniSHARC). No compressors (except miniSHARC, DDRC-88BM). No input PEQ on some devices.
- **No Dirac blocks:** No Dirac FIR management.
- **Simpler routing:** Fixed topology, no matrix mixer with polarity.

### What's more complex:
- **Fixed-point encoding:** Biquad coefficients must be in 5.23 fixed-point format, not float32. The codec is well-defined (minidsp-rs `fixed_point.rs`) but adds a conversion step.
- **Different mute/invert encoding:** Mute uses `0x00800000`/`0x00000000` instead of `2`/`1`. Invert uses signed fixed-point `0xFF800000`/`0x00800000`.
- **Delay as Int32 samples:** Instead of Int16 samples.
- **No metering on most:** m2x4, m4x10hd, m10x10hd have no level meters. NanoDigi has a single compressor RMS readback.
- **Symbol naming conventions differ:** Legacy uses `MuteNoSlewAlg{N}mute`, `Gain1940AlgNS{N}`, `PEQ_{ch}_{band}`, `BPF_{ch}_{group}`. SHARC uses `Mixer_R_C_status`, `DGain_{ch}_0`, `PEQ_{ch}_{band}`.

### Net assessment:
Legacy device support adds **one `if/else` in the dialect layer** (already designed) plus **static address table data** (already extracted). The protocol handling, command encoding, and high-level API are shared. This is NOT a significant engineering effort.

## What's Needed to Port

### For devices with config.xml source data (4x10HD, 10x10HD, NanoDigi 2x8, miniSHARC, DDRC-88BM, C-DSP 8x12):

1. **Obtain config.xml files** -- SigmaStudio project exports. Available in minidsp-rs `dev` branch at `devtools/src/codegen/{device}/config.xml`. These are device data files (MiniDSP's SigmaStudio exports), not minidsp-rs source code.
2. **Write our own extraction** -- parse config.xml `<item name="..." addr="...">` entries to build symbol→address maps. Simple XML parsing, ~100 lines.
3. **Build our own device definitions** -- map extracted addresses into our Device struct format. Same approach as SHARC devices (where we extract from Device Console XML).

Estimated effort per device: **30-60 minutes** extraction + definition, plus ~2 hours for the shared config.xml parser.

**We do NOT copy minidsp-rs's generated Rust files or codegen pipeline.** We use the same source data (SigmaStudio XML exports) and write our own code, consistent with the clean-room approach used for all SHARC devices.

### For miniDSP 2x4 (non-HD):

The config.xml and symbols exist but the codegen mod.rs has `inputs: vec![], outputs: vec![]`. The symbols clearly show the device structure:
- 2 outputs (ch 1-2) with 6 PEQ each + 8 crossover biquads + delay + invert
- 4 additional sub-outputs (ch 3-6) with similar structure
- No input channels exposed (stereo input, no input PEQ)

Effort to complete: **1-2 hours** to write the input/output mapping using existing symbol names, then generate.

### For nanoAVR / nanoAVR HDA:

No config.xml exists anywhere. These devices use HDMI source switching (command 0x49, not 0x34). Without a config.xml or a packet capture from a device owner, only basic support (volume/mute/source/preset) is possible. This matches minidsp-rs's current state.

## Probe Mapping (hw_id, dsp_version -> Device)

From minidsp-rs `dev` branch `protocol/src/device/probe.rs`:

```
(1, 51)   -> M10x10Hd
(1, _)    -> M4x10Hd
(2, 54)   -> Nanodigi2x8
(2, 22)   -> M2x4
(4, _)    -> MSharc4x8
(6, 95)   -> DDRC88BM
(10, 100) -> M2x4Hd
(10, 101) -> DDRC24
(11, 97)  -> C8x12v2
(14, _)   -> Shd
(27, 100) -> Flex
(27, 101) -> FlexDl
(32, 113) -> FlexHtx
_         -> Generic (basic support only)
```

Our additions:
```
(17, _)   -> Shd (SHD Studio, same as SHD)
(21, 105) -> ShdPower (SHD Power, same DSP as SHD, different source map)
(30, 110) -> FlexEight
(30, 111) -> FlexEightDl
(31, 113) -> FlexHt (same DSP as FlexHtx, different source map)
(31, 115) -> FlexHt (new FW with input PEQ)
(32, 115) -> FlexHtx (new FW with input PEQ)
```

## Current Project Status

### Implemented today

- SHARC-era devices covered by `docs/ref-address-tables.md` are implemented in `minidsp-open`.
- Legacy 4x10 HD, 10x10 HD, NanoDigi 2x8, miniSHARC 4x8, DDRC-88BM, and C-DSP 8x12 v2 are identified and supported at `DspKnown` tier in current code.
- miniDSP 2x4 non-HD, nanoAVR, and nanoAVR HDA remain basic-support devices.

### Still deferred or incomplete

- **PWR-ICE125/250** (hw_id 3) -- already deferred per FUTURE-pwr-ice.md
- **CDSP 8x12DL** -- need probe from device owner
- **hw_id 5** (10x10HD variant) -- probably shares address table with hw_id 1/dsp 51, but unconfirmed

### Follow-on work to reach full legacy parity

- Build the repeatable importer/enumeration pipeline from `app/app-extracted`, `app/apk-extracted`, and legacy `config.xml` sources
- Implement the clean-room SigmaStudio `config.xml` parser
- Extract and wire full address tables for Tier 1 legacy devices
- Promote legacy devices from `DspKnown`/basic support to full support as each device family is verified

### Effort estimate for legacy devices:

| Task | Effort |
|------|--------|
| Port dialect system (fixed_point.rs, 2-byte addr encoding) | 2-3 hours |
| Port 4x10HD address table | 30-60 min |
| Port 10x10HD address table | 30-60 min |
| Port NanoDigi 2x8 address table | 30-60 min |
| Port miniSHARC 4x8 address table | 30-60 min |
| Port DDRC-88BM address table | 30-60 min |
| Port C-DSP 8x12 v2 address table | 30-60 min |
| Wire up miniDSP 2x4 non-HD (basic -> full) | 1-2 hours |
| Add nanoAVR/HDA basic support stubs | 30 min |
| Add probe entries for all hw_ids | 30 min |
| **Total** | **~8-12 hours** |

This is entirely realistic for launch. The dialect layer is the only real engineering work; everything else is mechanical address table porting.

## Key Source Files to Port From

All from minidsp-rs `dev` branch (`https://github.com/mrene/minidsp-rs/tree/dev/`):

| File | Purpose | Size |
|------|---------|------|
| `protocol/src/dialect.rs` | Dialect abstraction | 2.4KB |
| `protocol/src/fixed_point.rs` | 5.23 fixed-point codec with tests | 4KB |
| `protocol/src/commands.rs` | Addr struct, Value enum with FixedPoint | 34KB |
| `protocol/src/device/probe.rs` | hw_id -> device mapping | 3.8KB |
| `devtools/src/codegen/m4x10hd/config.xml` | 4x10HD SigmaStudio XML | 42KB |
| `devtools/src/codegen/m10x10hd/config.xml` | 10x10HD SigmaStudio XML | 76KB |
| `devtools/src/codegen/nanodigi2x8/config.xml` | NanoDigi 2x8 SigmaStudio XML | 46KB |
| `devtools/src/codegen/msharc4x8/config.xml` | miniSHARC 4x8 SigmaStudio XML | 239KB |
| `devtools/src/codegen/ddrc88bm/config.xml` | DDRC-88BM SigmaStudio XML | 80KB |
| `devtools/src/codegen/c8x12v2/config.xml` | C-DSP 8x12 v2 SigmaStudio XML | 115KB |
| `devtools/src/codegen/m2x4/config.xml` | miniDSP 2x4 SigmaStudio XML | 51KB |

The generated .rs files (in `protocol/src/device/`) can be used directly as address references even if we don't replicate the codegen pipeline.
