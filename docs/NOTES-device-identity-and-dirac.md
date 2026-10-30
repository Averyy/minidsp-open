# Device Console identity boundary, selectable sources, and Dirac layout

Static research, 2026-10-29. No device, application session, firmware execution, or local capture was used. Only this note was added; existing documentation and code were left unchanged.

The inspected bundle identifies itself as **1.2.3**, not the 2.10.6 named in `docs/ref-address-tables.md` (`Contents/Info.plist`, lines 19–22; extracted `package.json`, line 4). Conclusions below concern these local assets.

## Evidence references and method

Paths below are repository-relative:

- **R** = `app/MiniDSP Device Console.app/Contents/Resources/`.
- **P** = `R/device_support_package/`.
- **J** = `app/app-extracted/build/src/`. Each cited JS file is also the identically named `build/src/` member of `R/app.asar`. Byte-for-byte comparison against the ASAR confirmed equality for `deviceSupportPackage.js`, `usbListener.js`, `deviceDefn.js`, `dspMessage.js`, `streamDevice.js`, `index.js`, `diracData.js`, `userPreset.js`, and `audioProcessingDefn.js`.
- **N** = `R/dirac/minidsp_dirac`, the packaged x86-64 Mach-O helper. Native addresses below are its unslid virtual addresses, not file offsets.

JS references are **zero-based byte offsets in the original, obfuscated file**, with exclusive ends. The string-table decoder/rotation was isolated to interpret property names; application modules were not run. Control flow and data accesses, rather than string presence alone, support the findings. XML references use original file line numbers. Native findings come from `nm -n -C` and `otool -tvV`, not decompilation. Ghidra instance discovery found no Device Console/helper project; no unrelated project was connected or modified, and no Ghidra program analysis was performed.

Useful JS locations for continuation:

| Reference | File, symbol, original byte interval |
|---|---|
| J1 | `J/deviceSupportPackage.js`: `initPackageFullPath` 15180–15631; `deviceIdList` 16114–16472; `isIntermediateDspId` 16472–17108 |
| J2 | Same file: `loadhwDefnDetails` 29243–35796; `loadDspDefnDetails` 36307–38845; `loadDspDefaultParams` 39360–57167; `loadDspStructure` 57654–66960; `loadTopLevelCfg` 69031–74834 |
| J3 | `J/usbListener.js`: `isMiniDSP` 10049–10290; `UsbDeviceSuppInfoFinder.getMissingOrDisconnect` 12955–15110 |
| J4 | `J/deviceDefn.js`: `getDefn` 3605–9198; `isDspIdValid` 9198–9708; `getAssociatedDeviceIds` 9708–10304 |
| J5 | `J/dspMessage.js`: `AudioSrcSelectMsg` and selector commands 28910–29850; `DspMsg.fromBuffer` 50692–70187, selector-reply branch near 56096; `GetDspIdDspCmd` starts at 16221 |
| J6 | `J/streamDevice.js`: `getSelectableAudioSrc` 20658–22039; `setSelectableAudioSrc` 22039–23197; `reloadE2promHeader` 30793–31095 |
| J7 | `J/index.js`: `createDiscoveredDevFromDspDevice` 66141–70372; selector IPC handlers start near 135419 and 136429; `checkCorrectInvalidDspId` 216381–219301; `createModelsByDspIds` 220207–220765; fallback-model IPC string call at 221528 |
| J8 | `J/diracData.js`: `diracDataValueToBuffer` 2614–4795; `J/userPreset.js`: `writeToDevice` 43748–45935 |
| J9 | `J/audioProcessingDefn.js`: `AudioProcessPipeline.isDiracSupported` 19505–19835; `uiDspStructFromAudioPipeline` 48054–51908 |

## Confirmed facts

### 1. Connected-device selection and the five unresolved products

Device Console has **separate USB admission and HW/DSP-definition catalogs**. `loadTopLevelCfg` reads `USB/DEVICE` into VID/PID pairs and independently reads `DEVICES/HW/DSP_STRUCTURES/DSP` into maps. USB comments/product labels do not become runtime HW/DSP mappings. `UsbDspListener.isMiniDSP` tests only membership in the VID/PID list. [J2, J3; `P/info.xml`, lines 16–29 and 30–191.]

The normal selection chain is:

1. Admit the HID interface by VID/PID.
2. Query `getPicVersionAndHardwareId()`, storing the returned hardware ID, then `getDspId()`, then the serial number. These are separate protocol results; the PID is not converted into a hardware ID. The DSP-ID command reads one byte at logical flash address `0xFFA1`. [J3; `J/streamDevice.js`, `getDspId` 14825–15120 and `getPicVersionAndHardwareId` 33367–33689; `J/dspMessage.js`, `GetDspIdDspCmd` starting at 16221.]
3. Call `getDefn(hwId, dspId)`. HW details come from `HW/{hwId}/info.xml`; DSP details, structure, and defaults come from `DSP_Structures/{dspId}/info.xml`, `DSP_structure_layout.xml`, and `DSP_default_parameters.xml`. Definitions are cached under that pair. No serial-range or product-string dispatch appears in this selection chain. USB audio names are stored as metadata on an already selected pair. [J2, J4, J7.]

The complete packaged HW/DSP catalog is `(10,100/101)`, `(27,100/101)`, `(30,110/111)`, `(31,113/115)`, `(32,113/115)`, `(14,105)`, `(17,105)`, `(21,105)`: **13 pairs**. There are no HW/DSP rows or per-product firmware definitions for the five labels below. This is a confirmed package boundary, not proof of what those physical products would report. [`P/info.xml`, lines 31–190; J1 `deviceIdList`, J2 `loadTopLevelCfg`.]

| Product label | Confirmed USB identity | Exact `hw_id`, DSP ID/version, firmware family in this package | Comparison with minidsp-open |
|---|---|---|---|
| HA-DSP | VID `0x2752`, PID `0x0013` | Not supplied by the HW/DSP catalog; no exact alias established | **(c) Exact runtime mapping missing**; PID admission already covered |
| DDRC-22 | `0x2752:0x0040` | No HW/DSP row. Native helper has a two-channel default Dirac path for this PID, without establishing a reported HW/DSP pair | **(c)**; no justified alias to DDRC-24 `(10,101)` |
| DDRC-88 | `0x2752:0x0043` | No HW/DSP row. Native helper has an eight-channel default Dirac path for this PID | **(c)**; existing DDRC-88BM `(6,95)` is a candidate, not a proved identity for every DDRC-88 variant |
| CDSP-8x12/DL | `0x2752:0x0047` | No HW/DSP row. Shares a native eight-channel Dirac setup path with PID `0x0048` | **(c)**; existing C-DSP 8x12 v2 `(11,97)` is not proved equivalent to the DL family |
| Harmony DSP | `0x2752:0x0048` | No HW/DSP row. Shares the preceding helper path; its USB product name is normalized as described below | **(c)** for runtime identity; a confirmed **name alias** does not establish a register-table alias |

PID evidence: `P/info.xml`, respectively lines **19, 20, 21, 24, 25**. Repo comparison: `docs/ref-devices.md`, USB registry and unresolved-product discussion; `protocol/src/device/generated_official.rs`, `OFFICIAL_USB_PRODUCT_HINTS` and 13 official identities; `protocol/src/device/probe.rs`, lines 19–65 and 71–100. None of the five can currently be promoted to **(a) exact** or **(b) safely interchangeable runtime identity** from these assets alone.

#### What the native helper actually proves

- PID `0x0040` is passed to `USBScan` at **N `0x10000bdc3–0x10000bdcd`**. That branch calls `get_dirac_info`, with a conditional `create_default_dirac_info(1,2,1,...)` fallback at **`0x10000bdf6–0x10000be1c`**. This supports the two-channel Dirac bucket, not an exact DSP ID.
- PID `0x0043` is scanned at **N `0x10000d952–0x10000d95c`**, followed by the corresponding eight-channel fallback at **`0x10000d985–0x10000d9ab`**.
- PIDs `0x0047` and `0x0048` converge at **N `0x100010a3d`**, after scans at **`0x100010a05–0x100010a30`**; the shared default is `create_default_dirac_info(1,8,1,...)` at **`0x100010a59–0x100010a7f`**. Sharing calibration setup does not prove shared DSP addresses or `hw_id`.
- The helper initializes a comparison string `Harmony DSP 8x12` at **N `0x10000bc82–0x10000bc97`**. It obtains a USB product name through `USBGetProductName` at **`0x10001df49–0x10001df58`**, compares its contents/length with that string at **`0x10001df95–0x10001e07e`**, and on equality assigns `PADSP-8x12-DL` at **`0x10001e080–0x10001e08e`**. This is an executed-code-backed **product-name normalization**, not just two nearby strings.
- Crucially, the “Failed to get dsp id, assume it is 113” message belongs to the **PID `0x004B` path**, whose scan is at **N `0x10000f112–0x10000f11c`**. That path reads full 24-bit address `0xFFFFA1` using command `0x3D` at **`0x10000f19e–0x10000f1bb`**, tests the returned DSP byte against `0x73` / 115 at **`0x10000f1f2–0x10000f1f9`**, and logs the failure text at **`0x10001502f`**. **These fallback strings do not establish Harmony as DSP 113/115.** The PID `0x0048` scan is in the separate path above.

The known DSP-ID aliases inside the JS catalog are firmware-conversion intermediates: `200→100` and `201→101` for HW 10/27, and `210→110` and `211→111` for HW 30. `isIntermediateDspId` looks for an explicit `intermediateId` under the same hardware ID. This is not a five-product alias map. [`P/info.xml`, lines 37, 45, 61, 69, 85, 93; J1.]

The fallback-model list is also not another product catalog: `createModelsByDspIds` groups `deviceSupportPkg.deviceIdList` by DSP ID. Invalid-DSP recovery chooses among definitions associated with the existing hardware ID and may reprogram firmware; it does not infer the missing products from their PIDs or serial ranges. [J4, J7.]

### 2. `0xFFEA` selectable-source layout

This is a **little-endian bitmap indexed by source ID**, with no count, version, or per-source record header. `AudioSrcSelectMsg` specifies high address `0xFF`, low address `0xEA`, and `maxNumSrcs=32`; the read/write constructors use normal `ReadFlash=0x05` and `WriteFlash=0x04`. The reply parser recognizes that address and passes bytes starting at reply offset 4 to the selector reader. [J5; `J/dspMessage.js`, command enum near bytes 3700–5500.]

| Location | Meaning |
|---|---|
| `0xFFEA + floor(source_id / 8)` | Byte containing that source's flag |
| Bit `source_id % 8` in that byte | `1`: selectable; `0`: not selectable |
| Up to four bytes | Source IDs 0–31, as declared by the app's maximum |

This layout follows from `getSelectableAudioSrc(count,start=0)`: request `ceil((start+count)/8)` bytes, read unsigned LE, shift right by `start`, then return successive low-bit tests. `setSelectableAudioSrc(flags,start=0)` constructs the same bit order, shifts left by `start`, and writes from `0xFFEA`. It **does not read/merge bits outside the supplied interval**. [J6.]

The current HT source maps fit in **one byte**:

| Bit | Flex HT, HW 31 | Flex HTx, HW 32 |
|---|---|---|
| 0 | No declared source | Analog Input |
| 1 | TOSLINK | TOSLINK |
| 2 | SPDIF | SPDIF |
| 3 | USB | USB |
| 4 | HDMI | HDMI |
| 5–7 | No declared source | No declared source |

Sources: `P/HW/31/info.xml`, lines 13–18; `P/HW/32/info.xml`, lines 13–19. The catalog associates both HW IDs with DSP 113 and 115 (`P/info.xml`, lines 103–140); the selector capability is HW metadata, not a DSP-ID test.

All declared sources enabled therefore encode as **`0x1E` for HT** and **`0x1F` for HTx**. For example, HT TOSLINK+HDMI is `0x12`. These are derived examples, not observed defaults. The reader requests `(count=4,start=1)` for HT and `(5,0)` for HTx because the IPC handler derives the interval from the minimum/maximum source IDs. After a successful write, the handler calls `reloadE2promHeader()` (opcode `0x24`). [J6, J7; J5 command enum and `GetReloadE2promHdrDspCmd` starting at byte 36561.]

The same selectable-source capability applies beyond HT:

| HW | Product | Declared source IDs | XML evidence |
|---|---|---|---|
| 14 | SHD | 0–6 | `P/HW/14/info.xml`, lines 12–20 |
| 17 | SHD Studio | 0–4 | `P/HW/17/info.xml`, lines 12–18 |
| 21 | SHD Power | 0–4 | `P/HW/21/info.xml`, lines 12–18 |
| 27 | Flex / Flex DL | 0–4 | `P/HW/27/info.xml`, lines 13–19 |
| 30 | Flex Eight / DL | 1–4 | `P/HW/30/info.xml`, lines 13–18 |
| 31 | Flex HT | 1–4 | `P/HW/31/info.xml`, lines 13–18 |
| 32 | Flex HTx | 0–4 | `P/HW/32/info.xml`, lines 13–19 |

Each row declares `selectable="true"`; `loadhwDefnDetails` parses it and the IPC handler uses the common selector methods. HW 10's source list has no such flag (`P/HW/10/info.xml`, lines 11–15). [J2, J6, J7.]

**Bounds and implementation limits:** the byte representation is 0–255, with current HT/HTx meaningful bits restricted to the masks above. The backend has no explicit “at least one source” check; firmware acceptance of zero is not established. The generic JS reader requests three bytes for source spans ending at IDs 16–23 but calls `readUInt32LE`; the writer uses four bytes for that case. Its bitwise construction also uses signed JS 32-bit operations, so the advertised 32-slot bound should not be mistaken for verified bit-31 write behavior. Current packaged source maps only require one byte. [J6.]

### 3. Advanced Dirac descriptors and encoding

#### Address units: preset data, not a proved live register map

The XML `<dirac addr="B" size="S" chns="C">` describes a block at byte offset **B within the preset-values image**, with **S bytes per Dirac channel** and **C channels**. A child `idx` is a **32-bit-word offset within that channel**. `loadDspDefaultParams` retains these values; `diracDataValueToBuffer` allocates `C*S` bytes and writes each field at `channel*S + idx*4`. `UserPreset.writeToDevice` pads the preceding parameters to `firstChnStartAddr=B`, concatenates the Dirac buffer, then writes the preset-values flash block. [J2, J8.]

Thus a field's preset-image byte offset is:

`B + channel_index * S + 4 * idx` (zero-based channel).

**Do not place XML `addr` directly into the normal runtime DSP-index table, or assume `B/4+idx` is a live Dirac address.** The preset-image relationship is confirmed; the corresponding live address space requires separate evidence. [J8; native memory-query evidence below.]

| DSP family | C | B, bytes | S, bytes/channel | FIR slots T | XML evidence |
|---|---:|---:|---:|---:|---|
| 100: 2x4 HD / Flex | — | — | — | — | No `<dirac>` in `P/DSP_Structures/100/DSP_default_parameters.xml` |
| 101: DDRC-24 / Flex DL | 2 | 1792 | 5124 | 1088 | `P/DSP_Structures/101/DSP_default_parameters.xml`, lines 11–33 |
| 105: SHD family | 2 | 1792 | 9476 | 2176 | `P/DSP_Structures/105/DSP_default_parameters.xml`, lines 11–33 |
| 110: Flex Eight | — | — | — | — | No `<dirac>` in `P/DSP_Structures/110/DSP_default_parameters.xml` |
| 111: Flex Eight DL | 2 | 3568 | 5124 | 1088 | `P/DSP_Structures/111/DSP_default_parameters.xml`, lines 15–37 |
| 113: HT/HTx | 8 | 6624 | 3332 | 640 | `P/DSP_Structures/113/DSP_default_parameters.xml`, lines 22–44 |
| 115: HT/HTx with input PEQ | 8 | 8224 | 3332 | 640 | `P/DSP_Structures/115/DSP_default_parameters.xml`, lines 22–44 |

Family names/pairs come from `P/info.xml`, lines 31–190. No corresponding XML descriptors for HA-DSP, DDRC-22, DDRC-88, or C-DSP/Harmony are supplied by this package; the modern layouts must not be assigned to them from channel count alone.

#### Requested fields

All scalar values below are parsed as integers and serialized with **`writeUInt32LE`**, not float conversion. Neither these elements nor their parser branches specify a `params_range` or range validation. The representable serialization domain is unsigned 32-bit; that does **not** establish the DSP's accepted semantic range. [J2 `loadDspDefaultParams`, especially offsets 43942–45600; J8 `diracDataValueToBuffer`.]

| Requested control | Actual vendor descriptor | Relative word index / byte offset | Defaults | Confirmed type and limits |
|---|---|---|---|---|
| `mux_bypass` | `<mux_bypass>` → JS `muxBypass` | 9 / 36 | 1 for all five Dirac layouts | Raw u32 LE; enum meanings/range are not specified |
| `dirac_delay` | `<dirac_delay>` → `diracDelay` | 10 / 40 | T: 1088, 2176, or 640 | Raw u32 LE; equals FIR slot count in every layout; unit/scaling and allowed runtime range are not specified here |
| `mixer_fir` | `<mixer_fir>` → `mixerFIR` | 11 / 44 | 0 | Raw u32 LE; no proved selector/gain/boolean interpretation or accepted range |
| `fir-ctrl` | `<fir><ctrl>` → `firCtrl` | 42 / 168 | T | Raw u32 LE; count-like default equals allocated FIR length; legal update range is not specified |
| `iir-ctrl` | **No such scalar descriptor** in these layouts | No justified address | — | `<iir>` has a coefficient bank and fraction, not `<ctrl>`; JS has `iirTaps` and no matching `iirCtrl` field |

XML locations for the first three controls: 101/105 lines **22–24**, 111 **26–28**, 113/115 **33–35**. FIR control: 101/105 line **27**, 111 **31**, 113/115 **38**. IIR elements: 101/105 **30–32**, 111 **34–36**, 113/115 **41–43**. These are the per-family authoritative descriptors, including absence of an IIR scalar control.

For convenient inspection, channel 0's **preset-image byte offsets** are:

| DSP | `mux_bypass` | `dirac_delay` | `mixer_fir` | FIR `ctrl` | IIR coefficients start |
|---|---:|---:|---:|---:|---:|
| 101 | 1828 | 1832 | 1836 | 1960 | 6316 |
| 105 | 1828 | 1832 | 1836 | 1960 | 10668 |
| 111 | 3604 | 3608 | 3612 | 3736 | 8092 |
| 113 | 6660 | 6664 | 6668 | 6792 | 9356 |
| 115 | 8260 | 8264 | 8268 | 8392 | 10956 |

These are arithmetic expansions of the cited XML and serializer formula, **not absolute flash addresses or confirmed live registers**. Add S for each subsequent channel.

#### Surrounding fields explain the layout but do not supply an `iir-ctrl`

Each channel contains two digital-gain statuses at word indices 0–1, two gains at 2–3, two meters at 4–5, a separate channel delay at 6, and two polarity values at 7–8. After the requested scalars, `mixer_iir` occupies **30 u32 words at 12–41**, followed by FIR control at 42. FIR coefficients occupy T words starting at 43. IIR coefficients start at `43+T`: 1131 for 101/111, 2219 for 105, 683 for 113/115. Their dimensions are **30×5 words**. The full channel stride is consequently `4*(T+193)`, matching every XML S above. [Same XML blocks; J2, J8.]

The default builder writes zeros to FIR taps except the **last tap**, which receives the raw word `0x3D042108`. It writes the same word into coefficient dimension 0 of each five-word IIR group, with other coefficients zero. The XML calls this `iirFraction`, `iirFractionLocation="end"`, and `iirFractionDim="0"`. IEEE-754 interpretation of that word is approximately **0.03225806355 (1/31)**; the serializer preserves its raw bits using `writeUInt32LE`. No additional coefficient normalization should be inferred from that constant alone. [J2, FIR/IIR branches around bytes 45600–49000; J8; XML FIR/IIR lines cited above.]

The neighboring `delay idx="6"` must not be confused with `dirac_delay idx="10"`. Likewise, `mixer_iir` is not evidence of a scalar `iir-ctrl`, and Dirac `firCtrl` is not the ordinary non-Dirac `FIR_*_Taps` parameter. [Cited XML blocks and J2/J8.]

#### Live memory descriptors are queried separately

The native helper's `RpcHandler::getFIIRmem` at **N `0x10005bbd0–0x10005bc87`** sends a **`0x39` command with a channel byte**, requires a nonzero reply with opcode `0x39`, and reads four **big-endian u16** fields from reply offsets **2, 4, 6, 8**. The same query is inlined in `downloadFilter` at **`0x100058234–0x100058291`**.

Following the result into `downloadFIR` and `downloadIIR` establishes this descriptor order:

| Reply offset | Meaning derived from use |
|---|---|
| 2–3 | FIR capacity/count, words |
| 4–5 | FIR live start address |
| 6–7 | IIR capacity/count, five-word groups |
| 8–9 | IIR live start address |

Evidence: conversion/passing of the first pair at **N `0x100058311–0x1000583b8`**, second pair at **`0x1000585ff–0x100058619`** and **`0x10005884e–0x100058865`**; `downloadFIR` multiplies capacity by four at **`0x10005bccd–0x10005bcd4`**, while `downloadIIR` multiplies by four and then five at **`0x10005bfee–0x10005bff6`**. These identify the descriptors without requiring a connected device, but actual returned addresses remain unknown.

Native coefficient transfers use **opcode `0x13` with selector byte `0x40`** and a BE address, as constructed in `downloadFIR` at **N `0x10005bd52–0x10005bd61`** and `downloadIIR` at **`0x10005c0a5–0x10005c0af`**. The JS ordinary parameter loader uses selectors `0x80` or `0xA0` instead (`J/dspMessage.js`, `GetLoadDspParamCmd` constructor, bytes 31341–31784). This is further reason to preserve the address-space distinction.

#### DSP 115 caveat

The inspected desktop package contains both the full Dirac data block for DSP 115 **and** a `DIRAC_FIIR` module marked `optional="true"` in `P/DSP_Structures/115/DSP_structure_layout.xml`, lines **319–340**. JS derives `diracSupported` and `diracOptional` from that module; it does not reject DSP 115 by number in this path. [J9.] Thus these desktop assets do **not** support a blanket claim that DSP 115 has no usable Dirac solely because an older Android list omitted it. Actual license/firmware activation is not established by the descriptor's presence either.

## Strong inferences

- DDRC-88BM `(6,95)` and C-DSP 8x12 v2 `(11,97)` remain plausible relatives of the corresponding USB families, but the helper's shared channel setup is insufficient to establish interchangeable source maps, preset formats, or addresses. The exact-pair catalog and selection chain above justify retaining them as candidates rather than adding aliases.
- `dirac_delay=T` is plausibly FIR processing latency in samples, and FIR `ctrl=T` plausibly selects an active tap count. Equal defaults and allocated length support those interpretations; no traced setter or explicit unit/range proves them. In particular, do not apply the normal channel-delay millisecond scaling to `dirac_delay` merely because of its name.
- Harmony's normalization to `PADSP-8x12-DL` establishes a shared naming boundary in the native Dirac endpoint. It may explain software-family reuse, but it does not identify the device as Flex HT or prove DSP 113/115. The separate PID paths provide stronger evidence than the earlier proximity of those strings.

## Unresolved questions

- Actual reported `hw_id`, DSP-ID byte, DSP firmware version, controller firmware version, and firmware family for each of the five products. No exact pair or complete firmware definition was found in this package. Resolving an identity requires a matching vendor definition/legacy plugin, a traced firmware constant/report, or a minimal owner probe that records identity fields without retaining personal identifiers.
- Semantic enums and accepted limits for `mux_bypass`, `mixer_fir`, `dirac_delay`, and FIR `ctrl`. The XML/parser supplies storage types and defaults, not those contracts. There is no evidenced standalone `iir-ctrl` descriptor to add.
- Per-family **live** addresses for the advanced scalar controls and FIR/IIR banks. XML gives the preset-image layout; `0x39` demonstrates that the native helper obtains live bank descriptors from firmware. No static result or device reply was available to equate those address spaces.
- Firmware behavior for empty source masks, undeclared bits, and source spans beyond the current one-byte maps. The generic JS edge cases should not be copied into a typed parser.

## Suggested next steps

1. Add a typed selectable-source **bitmap** parser at `0xFFEA`, using unsigned LE bits indexed by source ID and HW metadata for valid sources. Cover HT `0x1E`, HTx `0x1F`, and a partial mask such as HT `0x12`; validate payload length. Keep writing/reloading explicit, and decide whether a partial-update API preserves out-of-range bits, since the vendor writer does not.
2. Add importer metadata for Dirac **preset-block** base, byte stride, channel count, word offsets, and raw-u32 defaults for DSP 101/105/111/113/115. Keep these separate from live `DspTable` addresses; omit a fabricated `iir-ctrl`. A typed `0x39` descriptor is justified by the native consumer trace.
3. Retain all five PID hints, but add no guessed HW/DSP probe entries. Annotate Harmony's **name alias** and the shared native calibration path; remove any inference that the HT-only DSP-113 fallback identifies Harmony. Promote a product to an exact pair only after obtaining the evidence described above.
4. Review the existing Dirac address-unit and DSP-115 capability notes against this bundle. Preserve the distinction between desktop descriptor availability, license/firmware activation, and older Android capability lists.
