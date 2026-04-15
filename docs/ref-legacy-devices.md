# minidsp-open: Legacy Device Address Tables & Analysis

Research date: 2026-03-30. All data from minidsp-rs `dev` branch source code, MiniDSP Device Console, and miniDSP Controller Android APK.

Important status note:
- this document shows what legacy-device source data exists and how it maps into support
- legacy full support for the cached `config.xml` families is now implemented in the protocol crate
- the remaining next step is not more known legacy table ports; it is exact runtime identity for the remaining official USB-only aliases, if stronger official evidence than the current extracted corpus can be found

## Executive Summary

Complete address tables for every legacy device exist in SigmaStudio config.xml exports (the same files minidsp-rs uses as source data). These config.xml files are MiniDSP device data (SigmaStudio project exports), not minidsp-rs code. We use them as source data for our own device definitions, same as we use Device Console XML for SHARC devices.

**Status:** Source data exists locally in this repo under `third_party/legacy-configs/`, and the repeatable extractor now emits generated legacy runtime metadata into `docs/generated/device-support-matrix.json`, `docs/ref-device-coverage.md`, and `protocol/src/device/generated_official.rs`. `miniSHARC 4x8`, `DDRC-88BM`, `C-DSP 8x12 v2`, `10x10 HD`, `4x10 HD`, `miniDSP 2x4`, and `NanoDigi 2x8` are now promoted to source-backed runtime tables in `minidsp-open`. Treat this file as source-data and planning reference, not as a blanket claim that all-device parity is already shipped.

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
| **miniDSP 2x4** | 2 | 22 | Old | 2in/4out | 6 in, 6 out | 51KB | 34KB |
| **NanoDigi 2x8** | 2 | 54 | Old | 4in/10out* | 5 in, 5 out | 46KB | 33KB |
| **miniSHARC 4x8** | 4 | any | New** | 4in/8out | 10 in, 10 out | 239KB | 44KB |
| **DDRC-88BM** | 6 | 95 | New** | 8in/8out | 0 in, 10 out | 80KB | 62KB |
| **C-DSP 8x12 v2** | 11 | 97 | New** | 8in/12out | varies | 115KB | 79KB |

*miniDSP 2x4: the cached `config.xml` exposes two input PEQ banks plus four output PEQ/crossover/gain/mute/invert/delay groups. Our extractor now infers a real `2 in / 4 out` topology from the old `EQ_ch*` naming family, and `minidsp-open` now ships a source-backed runtime table for it. The older `0 in / 4 out` notes reflected incomplete upstream generated metadata, not the actual SigmaStudio source data.

*NanoDigi 2x8: the cached `config.xml` exposes four input PEQ banks, ten output PEQ/crossover/gain/mute/invert groups, and eight delay slots. `minidsp-open` now follows that wider source-backed internal topology instead of the older `2 / 8` assumption derived from the product name alone.

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
| **PWR-ICE125/250** | 3 (dsp 51) | Deferred per `docs/TODO4-pwr-ice-compatibility.md`. LAN-only, no config.xml, no address tables. |

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

1. **Obtain config.xml files** -- SigmaStudio project exports. These are now cached locally in `third_party/legacy-configs/{device}/config.xml`. The original public source is the minidsp-rs `dev` branch at `devtools/src/codegen/{device}/config.xml`. These are device data files (MiniDSP's SigmaStudio exports), not minidsp-rs source code.
2. **Write our own extraction** -- parse config.xml `<item name="..." addr="...">` entries plus `<filter>` / `<fir>` / `<label>` blocks to build symbol→address maps and runtime metadata. This now exists in first pass inside `tools/extract_device_support_matrix.py`.
3. **Build our own device definitions** -- map extracted addresses into our Device struct format. Same approach as SHARC devices (where we extract from Device Console XML).

Current generated guardrails from the cached legacy corpus:
- `protocol/tests/legacy_assets.rs` keeps the expected source-backed legacy families present in the generated matrix
- `protocol/tests/legacy_profile_metadata.rs` keeps legacy probe names/dialects/core dimensions/FIR flags aligned with generated source-backed metadata

This stage is complete for the cached corpus. Remaining work is no longer per-device table extraction; it is broader official-app identity coverage outside the exact XML/config.xml matrix.

**We do NOT copy minidsp-rs's generated Rust files or codegen pipeline.** We use the same source data (SigmaStudio XML exports) and write our own code, consistent with the clean-room approach used for all SHARC devices.

### For miniDSP 2x4 (non-HD):

The cached `config.xml` is sufficient to infer a real runtime table:
- 2 input PEQ banks (`EQ_ch1_1_*`, `EQ_ch2_1_*`)
- 4 output PEQ banks (`EQ_ch1_2_*`, `EQ_ch1_3_*`, `EQ_ch2_2_*`, `EQ_ch2_3_*`)
- 4 output crossover banks (`EQ_ch1_4/5_*`, `EQ_ch2_4/5_*`)
- output gain/mute/invert/delay at the old-dialect scalar slots near `352..367`

This is now implemented in `minidsp-open`, and the generated coverage artifacts infer the same `2 in / 4 out` topology from the source XML.

### For nanoAVR / nanoAVR HDA:

No config.xml exists anywhere. minidsp-rs historically claimed that HDMI source switching on these devices used opcode `0x49`, but that label was speculative and never verified on hardware. Device Console reverse engineering on 2026-04-11 (`app/app-extracted/build/src/dspMessage.js`) confirms `0x49` is actually `WriteDFlashId` — a paged write to the secondary D-flash — and the real HDMI / eARC source switching path is the `CMD_EARC` (0x53) sub-command family. Without a real packet capture from a nanoAVR owner, only basic support (volume/mute/source/preset) is possible, and any sightings of `0x49` in the wild for this family should be treated as D-flash writes, not source switches.

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
- Legacy `miniSHARC 4x8`, `DDRC-88BM`, and `C-DSP 8x12 v2` now have source-backed runtime tables and `Full` tier.
- Legacy `NanoDigi 2x8` is now source-backed `Full`, but its internal channel map follows the extracted `config.xml` (`4 / 10`) rather than the older product-name shorthand (`2x8`).
- miniDSP 2x4 non-HD is now source-backed `Full`.
- nanoAVR and nanoAVR HDA remain basic-support devices.
- The import work also fixed two shared runtime-model bugs:
  - FIR is no longer treated as input-only in the CLI/runtime path
  - output polarity is now optional in `DspTable`, matching source-backed devices that do not expose polarity on every output

### Still deferred or incomplete

- **PWR-ICE125/250** (hw_id 3) -- already deferred per `docs/TODO4-pwr-ice-compatibility.md`
- **CDSP 8x12DL / CDSP-8x12-DL** -- still only a USB product hint from official app assets; exact probe/runtime mapping still needed
- **hw_id 5** (10x10HD variant) -- probably shares address table with hw_id 1/dsp 51, but unconfirmed
- **HA-DSP** -- public product family is confirmed, and the public manual shows a dedicated `HA-DSP-CF` software family, but the extracted app assets still expose only the USB-product hint; native `minidsp_dirac` does not probe `PID 0x0013`, so exact probe/runtime mapping is still missing
- **DDRC-22** -- public `DDRC-22` family is confirmed, the public manual shows a dedicated `DDRC-22 Utility` software family, and native `minidsp_dirac` groups `PID 0x0040` into a `create_default_dirac_info(1, 2, 1, ...)` path shared with other small Dirac products; exact probe/runtime mapping is still missing
- **DDRC-88** -- public `DDRC-88A / DDRC-88D` family is confirmed, the public product/manual pages route it through the `DDRC-88BM` plugin family, and native `minidsp_dirac` groups `PID 0x0043` into a `create_default_dirac_info(1, 8, 1, ...)` path; `DDRC-88BM` is only the nearest implemented runtime profile, not the official identity
- **CDSP-8x12/DL** -- public `C-DSP 8x12 DL` family is confirmed; native `minidsp_dirac` disassembly shows the `PID 0x004b` Harmony path rewriting `Harmony DSP 8x12` to `PADSP-8x12-DL`, and the public Harmony manual confirms that both products share the `C-DSP 8x12 DL` plugin family, which keeps this in the same 8x12 custom-install bucket as `C-DSP 8x12 v2`
- **Harmony DSP** -- public `Harmony DSP 8x12` family is confirmed, native `minidsp_dirac` disassembly shows a dedicated `PID 0x004b` path that compares the HID product string against `Harmony DSP 8x12`, rewrites it to `PADSP-8x12-DL`, and later uses DSP-ID fallback around `113` / `115`, and the public manual explicitly says the `C-DSP 8x12 DL` plugin works with both Harmony DSP 8x12 and C-DSP 8x12 DL; exact probe/runtime mapping is still missing
- The older APK quick controller only branches on source-list `hw_id` values `[1, 2, 4, 5, 10, 11, 14, 17, 18, 27, 30, 31, 32]`, with `7/8` reserved for HDMI toggling. None of the unresolved USB-only labels appear there as standalone runtime `hw_id` families, which is useful negative evidence that they likely alias onto existing runtime buckets rather than missing hidden hardware rows in that older app logic.

### Follow-on work to reach full legacy parity

- Keep the repeatable importer/enumeration pipeline current across `app/app-extracted`, `app/apk-extracted`, and the cached legacy `config.xml` sources in `third_party/legacy-configs/`
- Resolve the remaining USB-hint product families into exact probe/runtime definitions only if stronger official evidence than the current extracted asset set is found
  - `HA-DSP`
  - `DDRC-22`
  - `DDRC-88`
  - `CDSP-8x12/DL`
  - `Harmony DSP`
- Do not treat the desktop fallback-model list as an unresolved source of extra identities; decoded bundle logic shows it is just `deviceSupportPkg.deviceIdList` grouped by `dspId`
- Do treat the exact HW directory list, exact DSP directory list, desktop `product-HW-DSP` image rows, and the exact desktop `userdocs/product-path.json` map as proof that the current packaged desktop assets do not contain hidden exact runtime rows for these aliases
- Do treat `userdocs/file-index.json` as a documentation-topic router only; it is sparse even for known modern products and is not a usable identity catalog

### Effort estimate for remaining legacy-adjacent work:

| Task | Effort |
|------|--------|
| Resolve USB-hint product families (`HA-DSP`, `DDRC-22`, `DDRC-88`, `CDSP-8x12/DL`, `Harmony DSP`) | partially narrowed: all five now have stable public-family naming, and four also have source-backed runtime-bucket hints; exact runtime identity is still unresolved |
| Desktop fallback-model catalog follow-up | no longer a blocker; decoded bundle logic shows it is just `deviceSupportPkg.deviceIdList` grouped by `dspId` |
| Revalidate shared-model changes on ESP32 after importer work | modest once shared identity boundary is stable |

The mechanical legacy-table promotion work is effectively done for the cached corpus. The remaining uncertainty is in official-app runtime identity extraction, not old-dialect address mapping.

## Key Source Files to Port From

All from minidsp-rs `dev` branch (`https://github.com/mrene/minidsp-rs/tree/dev/`):

| File | Purpose | Size |
|------|---------|------|
| `protocol/src/dialect.rs` | Dialect abstraction | 2.4KB |
| `protocol/src/fixed_point.rs` | 5.23 fixed-point codec with tests | 4KB |
| `protocol/src/commands.rs` | Addr struct, Value enum with FixedPoint | 34KB |
| `protocol/src/device/probe.rs` | hw_id -> device mapping | 3.8KB |
| `third_party/legacy-configs/m4x10hd/config.xml` | 4x10HD SigmaStudio XML | 42KB |
| `third_party/legacy-configs/m10x10hd/config.xml` | 10x10HD SigmaStudio XML | 76KB |
| `third_party/legacy-configs/nanodigi2x8/config.xml` | NanoDigi 2x8 SigmaStudio XML | 46KB |
| `third_party/legacy-configs/msharc4x8/config.xml` | miniSHARC 4x8 SigmaStudio XML | 239KB |
| `third_party/legacy-configs/ddrc88bm/config.xml` | DDRC-88BM SigmaStudio XML | 80KB |
| `third_party/legacy-configs/c8x12v2/config.xml` | C-DSP 8x12 v2 SigmaStudio XML | 115KB |
| `third_party/legacy-configs/m2x4/config.xml` | miniDSP 2x4 SigmaStudio XML | 51KB |

The generated .rs files (in `protocol/src/device/`) can be used directly as address references even if we don't replicate the codegen pipeline.
