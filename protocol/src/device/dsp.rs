//! DSP address tables for all device structures.
//!
//! All addresses from MiniDSP Device Console v2.10.6 DSP_default_parameters.xml.
//! Each address is a u16 index into DSP memory (4 bytes per index, float32 LE for new dialect).

/// PEQ band addresses (5 contiguous addresses per band: b0, b1, b2, a1, a2).
#[derive(Debug, Clone, Copy)]
pub struct PeqBand {
    /// Base address of the 5 biquad coefficients.
    pub addr: u16,
}

impl PeqBand {
    pub const fn new(addr: u16) -> Self {
        Self { addr }
    }
}

/// Crossover filter addresses (LPF and HPF, each 20 addresses for up to 8th order).
#[derive(Debug, Clone, Copy)]
pub struct Crossover {
    pub lpf_addr: u16,
    pub hpf_addr: u16,
}

/// Compressor addresses.
#[derive(Debug, Clone, Copy)]
pub struct Compressor {
    pub bypass: u16,
    pub threshold: u16,
    pub gain: u16,
    pub ratio: u16,
    pub knee: u16,
    pub attack: u16,
    pub release: u16,
}

/// FIR filter addresses.
#[derive(Debug, Clone, Copy)]
pub struct FirFilter {
    pub index: u8,
    pub bypass: u16,
    pub taps_count: u16,
    pub data_start: u16,
    pub max_taps: u16,
}

/// Output channel definition with all DSP parameter addresses.
#[derive(Debug, Clone, Copy)]
pub struct OutputChannel {
    pub mute: u16,
    pub gain: u16,
    pub delay: Option<u16>,
    pub polarity: Option<u16>,
    pub peq: &'static [PeqBand],
    pub fir: Option<FirFilter>,
    pub crossover: Option<Crossover>,
    pub compressor: Option<Compressor>,
}

/// Input channel definition.
#[derive(Debug, Clone, Copy)]
pub struct InputChannel {
    pub mute: Option<u16>,
    pub gain: Option<u16>,
    pub polarity: Option<u16>,
    pub peq: &'static [PeqBand],
    pub fir: Option<FirFilter>,
}

/// Routing matrix definition.
#[derive(Debug, Clone, Copy)]
pub struct RoutingMatrix {
    pub status_base: u16,
    pub gain_base: u16,
    pub polarity_base: Option<u16>,
    pub rows: u8,
    pub cols: u8,
}

/// Meter addresses.
#[derive(Debug, Clone)]
pub struct Meters {
    pub input: &'static [u16],
    pub output: &'static [u16],
    pub compressor: &'static [u16],
    pub dirac_in: &'static [u16],
    pub dirac_out: &'static [u16],
}

/// Dirac block definition.
#[derive(Debug, Clone, Copy)]
pub struct DiracBlock {
    pub base_addr: u16,
    pub size: u16,
    pub channels: u8,
    pub fir_taps: u16,
}

/// Bass management block (DSP 113/115).
#[derive(Debug, Clone)]
pub struct BassManagement {
    pub global_lpf_addr: u16,
    pub global_hpf_addr: u16,
    /// Base address of the per-channel `BM_DGain_*_status` words (8 entries).
    pub gain_status_base: u16,
    /// Base address of the per-channel `BM_DGain_*` gain values (8 entries).
    pub gain_base: u16,
    pub per_channel_bpf: &'static [(u16, u16)], // (lpf, hpf) per channel
    /// 9 × 8 bass-management routing matrix. The 9th row is the LFE virtual
    /// channel from the `LFE_CHN STANDALONE_DSP_MODULE`. Each cell has its own
    /// status, gain, and polarity address. See
    /// `protocol/assets/audio_firmware/113/DSP_default_parameters.xml` for the
    /// canonical layout.
    pub bm_routing: BmMatrixMixer,
}

/// 9 × 8 bass-management matrix mixer (`BM_MATRIX_MIXER`) shared by HT/HTx
/// firmware (DSP 113 and 115). The 9th input row is the LFE virtual channel
/// declared as a `STANDALONE_DSP_MODULE type="LFE_CHN" CHN_NUM="9"` in the
/// official structure layout.
///
/// Each cell occupies one parameter index in three contiguous regions
/// (status / gain / polarity), strided row-major. Row indices are 1-based in
/// the XML name (`BM_Mixer_R_C_*`); the address math here is 0-based.
#[derive(Debug, Clone, Copy)]
pub struct BmMatrixMixer {
    /// Total rows including the LFE virtual row at index 8 (= row 9 in XML).
    pub rows: u8,
    /// Output column count.
    pub cols: u8,
    /// Base address of `BM_Mixer_R_C_status` words (`rows * cols` entries,
    /// row-major). Status semantics match the regular routing matrix:
    /// `2 = enabled`, `1 = disabled`, `3 = ?` (rare in defaults).
    pub status_base: u16,
    /// Base address of `BM_Mixer_R_C` gain values (`rows * cols` entries),
    /// stored as float32 dB.
    pub gain_base: u16,
    /// Base address of `BM_Mixer_R_C_pol` polarity words (`rows * cols`
    /// entries), stored as 1-byte-per-cell integers (0 = normal, 1 = inverted).
    pub polarity_base: u16,
}

impl BmMatrixMixer {
    /// Number of cells in the matrix.
    pub const fn cell_count(&self) -> u16 {
        self.rows as u16 * self.cols as u16
    }

    /// Compute the cell offset (`row * cols + col`) for 0-based row, col.
    /// Returns `None` if the indices are out of range.
    pub const fn cell_offset(&self, row: u8, col: u8) -> Option<u16> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some((row as u16) * (self.cols as u16) + (col as u16))
    }

    /// Address of the status word for cell (row, col).
    pub const fn status_addr(&self, row: u8, col: u8) -> Option<u16> {
        match self.cell_offset(row, col) {
            Some(off) => Some(self.status_base + off),
            None => None,
        }
    }

    /// Address of the gain word for cell (row, col).
    pub const fn gain_addr(&self, row: u8, col: u8) -> Option<u16> {
        match self.cell_offset(row, col) {
            Some(off) => Some(self.gain_base + off),
            None => None,
        }
    }

    /// Address of the polarity word for cell (row, col).
    pub const fn polarity_addr(&self, row: u8, col: u8) -> Option<u16> {
        match self.cell_offset(row, col) {
            Some(off) => Some(self.polarity_base + off),
            None => None,
        }
    }

    /// True if `row` is the LFE virtual row (index `rows - 1` for HT
    /// families). Useful for clients that want to render an LFE-aware UI.
    pub const fn is_lfe_row(&self, row: u8) -> bool {
        row + 1 == self.rows
    }
}

/// Per-family delay range in milliseconds. Derived from the official
/// `DSP_default_parameters.xml` delay `<range>`/`<step>` metadata per family.
/// Used to clamp HTTP/CLI delay writes so invalid out-of-range floats never
/// reach DSP memory. See `docs/ref-address-tables.md` for per-family values
/// and `protocol/assets/audio_firmware/{family}/DSP_default_parameters.xml`
/// for source.
#[derive(Debug, Clone, Copy)]
pub struct DelayRange {
    /// Minimum delay in milliseconds (always 0 for shipped families).
    pub min_ms: f32,
    /// Maximum delay in milliseconds, inclusive.
    pub max_ms: f32,
    /// Quantization step in milliseconds (hardware snaps to integer multiples).
    pub step_ms: f32,
}

impl DelayRange {
    /// Conservative fallback used for tables without explicit XML-derived
    /// values. 30 ms / step 0.02 is the tightest of the seven modern families
    /// and matches the official 48 kHz Dirac family ranges.
    pub const fn fallback() -> Self {
        Self {
            min_ms: 0.0,
            max_ms: 30.0,
            step_ms: 0.02,
        }
    }

    /// Clamp `value_ms` into `[min_ms, max_ms]`.
    ///
    /// NaN is treated as the minimum (refuse to propagate garbage). `+∞` and
    /// `-∞` saturate to max and min respectively — this matches the C-level
    /// clamp semantics of `f32::clamp`, which would also saturate, but we
    /// handle it explicitly so NaN can't slip through as a payload.
    pub fn clamp(&self, value_ms: f32) -> f32 {
        if value_ms.is_nan() {
            return self.min_ms;
        }
        value_ms.clamp(self.min_ms, self.max_ms)
    }

    /// Return true if `value_ms` is finite and inside `[min_ms, max_ms]`.
    pub fn contains(&self, value_ms: f32) -> bool {
        value_ms.is_finite() && value_ms >= self.min_ms && value_ms <= self.max_ms
    }
}

/// Complete DSP address table for a device structure.
#[derive(Debug, Clone)]
pub struct DspTable {
    pub inputs: &'static [InputChannel],
    pub outputs: &'static [OutputChannel],
    pub routing: RoutingMatrix,
    pub meters: &'static Meters,
    pub dirac: Option<DiracBlock>,
    pub bass_management: Option<&'static BassManagement>,
    /// Per-family delay range in milliseconds. Used to clamp delay writes.
    pub delay_range: DelayRange,
    /// Total param-memory budget in 32-bit words, sourced from the
    /// `<params addr="0" size="N">` element in each family's
    /// `DSP_default_parameters.xml`. For Dirac-capable families the Dirac
    /// block starts immediately after this range. Used to bounds-check
    /// address reads/writes and to size full-config reads against the
    /// authoritative DSP memory layout rather than relying on the max address
    /// observed in the PEQ / routing tables. 0 = not populated / unknown.
    pub param_memory_size: u32,
    /// Protocol command-set version declared by `<CMD_VERSION>` in the
    /// official root `info.xml` DSP entry. All seven modern SHARC families
    /// currently use version 3. Recorded here so a future firmware bump to
    /// CMD 4 on one family (without the others) would flip a gate instead
    /// of silently misdecoding responses.
    pub cmd_version: u8,
    /// Intermediate DSP id reported while a firmware convert is in progress.
    /// Pulled from per-family `info.xml` `<DSP_FW_CONVERT intermediateDspId="...">`.
    /// `Some(n)` means "if a device in this family reports dsp_id = n, it's
    /// mid-convert to the sibling Dirac/non-Dirac firmware and will re-enumerate
    /// shortly" — see RE-NOTES §"100 <-> 101" for the exact pairings.
    pub intermediate_dsp_id: Option<u16>,
}

// ========================================================================
// DSP 100 — 2x4 HD / Flex (2 in / 4 out, 96kHz, FIR on output)
// ========================================================================

static DSP100_INPUT_PEQ_0: [PeqBand; 10] = peq_bands_10(8280);
static DSP100_INPUT_PEQ_1: [PeqBand; 10] = peq_bands_10(8330);

static DSP100_INPUT_0: InputChannel = InputChannel {
    mute: Some(0),
    gain: Some(26),
    polarity: None,
    peq: &DSP100_INPUT_PEQ_0,
    fir: None,
};

static DSP100_INPUT_1: InputChannel = InputChannel {
    mute: Some(1),
    gain: Some(27),
    polarity: None,
    peq: &DSP100_INPUT_PEQ_1,
    fir: None,
};

static DSP100_INPUTS: [InputChannel; 2] = [DSP100_INPUT_0, DSP100_INPUT_1];

// DSP 100 is defined in Device Console XML / upstream minidsp-rs with the
// compact register map below, not the older 4xxx/6xxx/8xxx synthetic table.
static DSP100_OUT0_PEQ: [PeqBand; 10] = peq_bands_10(8380);
static DSP100_OUT1_PEQ: [PeqBand; 10] = peq_bands_10(8430);
static DSP100_OUT2_PEQ: [PeqBand; 10] = peq_bands_10(8480);
static DSP100_OUT3_PEQ: [PeqBand; 10] = peq_bands_10(8530);

static DSP100_OUTPUTS: [OutputChannel; 4] = [
    OutputChannel {
        mute: 2,
        gain: 28,
        delay: Some(64),
        polarity: Some(80),
        peq: &DSP100_OUT0_PEQ,
        fir: Some(FirFilter {
            index: 0,
            bypass: 14,
            taps_count: 84,
            data_start: 85,
            max_taps: 2048,
        }),
        crossover: Some(Crossover {
            lpf_addr: 8580,
            hpf_addr: 8600,
        }),
        compressor: Some(Compressor {
            bypass: 22,
            threshold: 40,
            gain: 41,
            ratio: 42,
            knee: 43,
            attack: 44,
            release: 45,
        }),
    },
    OutputChannel {
        mute: 3,
        gain: 29,
        delay: Some(65),
        polarity: Some(81),
        peq: &DSP100_OUT1_PEQ,
        fir: Some(FirFilter {
            index: 1,
            bypass: 15,
            taps_count: 2133,
            data_start: 2134,
            max_taps: 2048,
        }),
        crossover: Some(Crossover {
            lpf_addr: 8620,
            hpf_addr: 8640,
        }),
        compressor: Some(Compressor {
            bypass: 23,
            threshold: 46,
            gain: 47,
            ratio: 48,
            knee: 49,
            attack: 50,
            release: 51,
        }),
    },
    OutputChannel {
        mute: 4,
        gain: 30,
        delay: Some(66),
        polarity: Some(82),
        peq: &DSP100_OUT2_PEQ,
        fir: Some(FirFilter {
            index: 2,
            bypass: 16,
            taps_count: 4182,
            data_start: 4183,
            max_taps: 2048,
        }),
        crossover: Some(Crossover {
            lpf_addr: 8660,
            hpf_addr: 8680,
        }),
        compressor: Some(Compressor {
            bypass: 24,
            threshold: 52,
            gain: 53,
            ratio: 54,
            knee: 55,
            attack: 56,
            release: 57,
        }),
    },
    OutputChannel {
        mute: 5,
        gain: 31,
        delay: Some(67),
        polarity: Some(83),
        peq: &DSP100_OUT3_PEQ,
        fir: Some(FirFilter {
            index: 3,
            bypass: 17,
            taps_count: 6231,
            data_start: 6232,
            max_taps: 2048,
        }),
        crossover: Some(Crossover {
            lpf_addr: 8700,
            hpf_addr: 8720,
        }),
        compressor: Some(Compressor {
            bypass: 25,
            threshold: 58,
            gain: 59,
            ratio: 60,
            knee: 61,
            attack: 62,
            release: 63,
        }),
    },
];

static DSP100_METERS: Meters = Meters {
    input: &[68, 69],
    output: &[74, 75, 76, 77],
    compressor: &[70, 71, 72, 73],
    dirac_in: &[],
    dirac_out: &[],
};

pub static DSP_100: DspTable = DspTable {
    inputs: &DSP100_INPUTS,
    outputs: &DSP100_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 6,
        gain_base: 32,
        polarity_base: None,
        rows: 2,
        cols: 4,
    },
    meters: &DSP100_METERS,
    dirac: None,
    bass_management: None,
    // 2x4 HD / Flex @ 96 kHz: 0 .. 80 ms step 0.01 (official DSP_default_parameters.xml).
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 80.0,
        step_ms: 0.01,
    },
    // `<params addr="0" size="34960">` — largest modern family (2x4HD has
    // FIR output taps at 85/2134/4183/6232 and full PEQ + crossover beyond).
    param_memory_size: 34960,
    cmd_version: 3,
    // 2x4 HD ↔ DDRC-24 conversion path per `100/info.xml` DSP_FW_CONVERT.
    intermediate_dsp_id: Some(201),
};

// ========================================================================
// DSP 101 — DDRC-24 / Flex DL (2 in / 4 out, 48kHz, Dirac)
// Shares output layout with DSP 100 but different base addresses.
// ========================================================================

static DSP101_OUT0_PEQ: [PeqBand; 10] = peq_bands_10(46);
static DSP101_OUT1_PEQ: [PeqBand; 10] = peq_bands_10(150);
static DSP101_OUT2_PEQ: [PeqBand; 10] = peq_bands_10(254);
static DSP101_OUT3_PEQ: [PeqBand; 10] = peq_bands_10(358);

static DSP101_OUTPUTS: [OutputChannel; 4] = [
    OutputChannel {
        mute: 32,
        gain: 34,
        delay: Some(41),
        polarity: Some(45),
        peq: &DSP101_OUT0_PEQ,
        fir: None,
        crossover: Some(Crossover {
            lpf_addr: 96,
            hpf_addr: 116,
        }),
        compressor: Some(Compressor {
            bypass: 33,
            threshold: 35,
            gain: 36,
            ratio: 37,
            knee: 38,
            attack: 39,
            release: 40,
        }),
    },
    OutputChannel {
        mute: 136,
        gain: 138,
        delay: Some(145),
        polarity: Some(149),
        peq: &DSP101_OUT1_PEQ,
        fir: None,
        crossover: Some(Crossover {
            lpf_addr: 200,
            hpf_addr: 220,
        }),
        compressor: Some(Compressor {
            bypass: 137,
            threshold: 139,
            gain: 140,
            ratio: 141,
            knee: 142,
            attack: 143,
            release: 144,
        }),
    },
    OutputChannel {
        mute: 240,
        gain: 242,
        delay: Some(249),
        polarity: Some(253),
        peq: &DSP101_OUT2_PEQ,
        fir: None,
        crossover: Some(Crossover {
            lpf_addr: 304,
            hpf_addr: 324,
        }),
        compressor: Some(Compressor {
            bypass: 241,
            threshold: 243,
            gain: 244,
            ratio: 245,
            knee: 246,
            attack: 247,
            release: 248,
        }),
    },
    OutputChannel {
        mute: 344,
        gain: 346,
        delay: Some(353),
        polarity: Some(357),
        peq: &DSP101_OUT3_PEQ,
        fir: None,
        crossover: Some(Crossover {
            lpf_addr: 408,
            hpf_addr: 428,
        }),
        compressor: Some(Compressor {
            bypass: 345,
            threshold: 347,
            gain: 348,
            ratio: 349,
            knee: 350,
            attack: 351,
            release: 352,
        }),
    },
];

static DSP101_METERS: Meters = Meters {
    input: &[],
    output: &[28, 29, 30, 31],
    compressor: &[24, 25, 26, 27],
    dirac_in: &[4, 5],
    dirac_out: &[],
};

pub static DSP_101: DspTable = DspTable {
    inputs: &[], // No input PEQ on DSP 101
    outputs: &DSP101_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 8,
        polarity_base: None,
        rows: 2,
        cols: 4,
    },
    meters: &DSP101_METERS,
    // Dirac block starts immediately after the main param region. Values
    // match `protocol/assets/audio_firmware/101/DSP_default_parameters.xml`
    // `<dirac>`.
    dirac: Some(DiracBlock {
        base_addr: 1792,
        size: 5124,
        channels: 2,
        fir_taps: 1088,
    }),
    bass_management: None,
    // DDRC-24 / Flex DL @ 48 kHz: 0 .. 30 ms step 0.02.
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 30.0,
        step_ms: 0.02,
    },
    // `<params addr="0" size="1792">`. Dirac block follows at idx 1792.
    param_memory_size: 1792,
    cmd_version: 3,
    // DDRC-24 ↔ 2x4 HD conversion path per `101/info.xml` DSP_FW_CONVERT.
    intermediate_dsp_id: Some(200),
};

// ========================================================================
// DSP 105 — SHD / SHD Studio / SHD Power (2 in / 4 out, 96kHz, Dirac)
// Same output layout as DSP 101 (same addresses 0-495).
// ========================================================================

pub static DSP_105: DspTable = DspTable {
    inputs: &[],              // No input PEQ on SHD (Dirac handles input)
    outputs: &DSP101_OUTPUTS, // Same output addresses as DSP 101
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 8,
        polarity_base: None,
        rows: 2,
        cols: 4,
    },
    meters: &DSP101_METERS,
    // Dirac block per
    // `protocol/assets/audio_firmware/105/DSP_default_parameters.xml`.
    // SHD reuses the 2/4 output layout of DSP 101 but its Dirac block is
    // larger (more FIR taps for the longer 96 kHz impulse response).
    dirac: Some(DiracBlock {
        base_addr: 1792,
        size: 9476,
        channels: 2,
        fir_taps: 2176,
    }),
    bass_management: None,
    // SHD / SHD Studio / SHD Power @ 96 kHz: 0 .. 30 ms step 0.01.
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 30.0,
        step_ms: 0.01,
    },
    // `<params addr="0" size="1792">`. Dirac block follows at idx 1792 with a
    // larger 9476-word footprint than 101's 5124 (SHD convolves 2176 taps).
    param_memory_size: 1792,
    cmd_version: 3,
    // SHD family has no documented convert path in `105/info.xml`.
    intermediate_dsp_id: None,
};

// ========================================================================
// DSP 110 — Flex Eight (2 in / 8 out, 96kHz, FIR on input)
// ========================================================================

static DSP110_INPUT_PEQ_0: [PeqBand; 10] = peq_bands_10(2120);
static DSP110_INPUT_PEQ_1: [PeqBand; 10] = peq_bands_10(4224);

static DSP110_INPUTS: [InputChannel; 2] = [
    InputChannel {
        mute: Some(66),
        gain: Some(68),
        polarity: Some(70),
        peq: &DSP110_INPUT_PEQ_0,
        fir: Some(FirFilter {
            index: 0,
            bypass: 67,
            taps_count: 71,
            data_start: 72,
            max_taps: 2048,
        }),
    },
    InputChannel {
        mute: Some(2170),
        gain: Some(2172),
        polarity: Some(2174),
        peq: &DSP110_INPUT_PEQ_1,
        fir: Some(FirFilter {
            index: 1,
            bypass: 2171,
            taps_count: 2175,
            data_start: 2176,
            max_taps: 2048,
        }),
    },
];

// Output channels stride 103, starting at 4274
macro_rules! dsp110_output {
    ($base:expr, $peq:expr) => {
        OutputChannel {
            mute: $base,
            gain: $base + 2,
            delay: Some($base + 9),
            polarity: Some($base + 12),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $base + 63,
                hpf_addr: $base + 83,
            }),
            compressor: Some(Compressor {
                bypass: $base + 1,
                threshold: $base + 3,
                gain: $base + 4,
                ratio: $base + 5,
                knee: $base + 6,
                attack: $base + 7,
                release: $base + 8,
            }),
        }
    };
}

static DSP110_OUT_PEQ: [[PeqBand; 10]; 8] = [
    peq_bands_10(4287),
    peq_bands_10(4390),
    peq_bands_10(4493),
    peq_bands_10(4596),
    peq_bands_10(4699),
    peq_bands_10(4802),
    peq_bands_10(4905),
    peq_bands_10(5008),
];

static DSP110_OUTPUTS: [OutputChannel; 8] = [
    dsp110_output!(4274, &DSP110_OUT_PEQ[0]),
    dsp110_output!(4377, &DSP110_OUT_PEQ[1]),
    dsp110_output!(4480, &DSP110_OUT_PEQ[2]),
    dsp110_output!(4583, &DSP110_OUT_PEQ[3]),
    dsp110_output!(4686, &DSP110_OUT_PEQ[4]),
    dsp110_output!(4789, &DSP110_OUT_PEQ[5]),
    dsp110_output!(4892, &DSP110_OUT_PEQ[6]),
    dsp110_output!(4995, &DSP110_OUT_PEQ[7]),
];

static DSP110_METERS: Meters = Meters {
    input: &[48, 49],
    output: &[58, 59, 60, 61, 62, 63, 64, 65],
    compressor: &[50, 51, 52, 53, 54, 55, 56, 57],
    dirac_in: &[],
    dirac_out: &[],
};

pub static DSP_110: DspTable = DspTable {
    inputs: &DSP110_INPUTS,
    outputs: &DSP110_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 16,
        polarity_base: Some(32),
        rows: 2,
        cols: 8,
    },
    meters: &DSP110_METERS,
    dirac: None,
    bass_management: None,
    // Flex Eight @ 96 kHz: 0 .. 60 ms step 0.01.
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 60.0,
        step_ms: 0.01,
    },
    // `<params addr="0" size="20392">`. Large footprint due to input-side FIR
    // blocks (2 × 2048 taps) plus 8-output PEQ/crossover chains.
    param_memory_size: 20392,
    cmd_version: 3,
    // Flex Eight ↔ Flex Eight DL conversion path per `110/info.xml`.
    intermediate_dsp_id: Some(211),
};

// ========================================================================
// DSP 111 — Flex Eight DL (2 in / 8 out, 48kHz, Dirac)
// ========================================================================

// Output stride 104
macro_rules! dsp111_output {
    ($base:expr, $peq:expr) => {
        OutputChannel {
            mute: $base,
            gain: $base + 2,
            delay: Some($base + 9),
            polarity: Some($base + 13),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $base + 64,
                hpf_addr: $base + 84,
            }),
            compressor: Some(Compressor {
                bypass: $base + 1,
                threshold: $base + 3,
                gain: $base + 4,
                ratio: $base + 5,
                knee: $base + 6,
                attack: $base + 7,
                release: $base + 8,
            }),
        }
    };
}

static DSP111_OUT_PEQ: [[PeqBand; 10]; 8] = [
    peq_bands_10(74),
    peq_bands_10(178),
    peq_bands_10(282),
    peq_bands_10(386),
    peq_bands_10(490),
    peq_bands_10(594),
    peq_bands_10(698),
    peq_bands_10(802),
];

static DSP111_OUTPUTS: [OutputChannel; 8] = [
    dsp111_output!(60, &DSP111_OUT_PEQ[0]),
    dsp111_output!(164, &DSP111_OUT_PEQ[1]),
    dsp111_output!(268, &DSP111_OUT_PEQ[2]),
    dsp111_output!(372, &DSP111_OUT_PEQ[3]),
    dsp111_output!(476, &DSP111_OUT_PEQ[4]),
    dsp111_output!(580, &DSP111_OUT_PEQ[5]),
    dsp111_output!(684, &DSP111_OUT_PEQ[6]),
    dsp111_output!(788, &DSP111_OUT_PEQ[7]),
];

static DSP111_METERS: Meters = Meters {
    input: &[],
    output: &[52, 53, 54, 55, 56, 57, 58, 59],
    compressor: &[44, 45, 46, 47, 48, 49, 50, 51],
    dirac_in: &[32, 33],
    dirac_out: &[34, 35],
};

pub static DSP_111: DspTable = DspTable {
    inputs: &[], // No input PEQ on DL variant
    outputs: &DSP111_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 16,
        polarity_base: None,
        rows: 2,
        cols: 8,
    },
    meters: &DSP111_METERS,
    dirac: Some(DiracBlock {
        base_addr: 3568,
        size: 5124,
        channels: 2,
        fir_taps: 1088,
    }),
    bass_management: None,
    // Flex Eight DL @ 48 kHz: 0 .. 30 ms step 0.02.
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 30.0,
        step_ms: 0.02,
    },
    // `<params addr="0" size="3568">`. Dirac block follows at idx 3568.
    param_memory_size: 3568,
    cmd_version: 3,
    // Flex Eight DL ↔ Flex Eight conversion path per `111/info.xml`.
    intermediate_dsp_id: Some(210),
};

// ========================================================================
// DSP 113 — Flex HT/HTx old FW (8 in / 8 out, 48kHz, bass mgmt, Dirac capable)
// ========================================================================

// Output stride 104, starting at 824
macro_rules! dsp113_output {
    ($base:expr, $peq:expr) => {
        OutputChannel {
            mute: $base,
            gain: $base + 2,
            delay: Some($base + 9),
            polarity: Some($base + 13),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $base + 64,
                hpf_addr: $base + 84,
            }),
            compressor: Some(Compressor {
                bypass: $base + 1,
                threshold: $base + 3,
                gain: $base + 4,
                ratio: $base + 5,
                knee: $base + 6,
                attack: $base + 7,
                release: $base + 8,
            }),
        }
    };
}

static DSP113_OUT_PEQ: [[PeqBand; 10]; 8] = [
    peq_bands_10(838),
    peq_bands_10(942),
    peq_bands_10(1046),
    peq_bands_10(1150),
    peq_bands_10(1254),
    peq_bands_10(1358),
    peq_bands_10(1462),
    peq_bands_10(1566),
];

static DSP113_OUTPUTS: [OutputChannel; 8] = [
    dsp113_output!(824, &DSP113_OUT_PEQ[0]),
    dsp113_output!(928, &DSP113_OUT_PEQ[1]),
    dsp113_output!(1032, &DSP113_OUT_PEQ[2]),
    dsp113_output!(1136, &DSP113_OUT_PEQ[3]),
    dsp113_output!(1240, &DSP113_OUT_PEQ[4]),
    dsp113_output!(1344, &DSP113_OUT_PEQ[5]),
    dsp113_output!(1448, &DSP113_OUT_PEQ[6]),
    dsp113_output!(1552, &DSP113_OUT_PEQ[7]),
];

static DSP113_BM_BPF: [(u16, u16); 8] = [
    (272, 292),
    (312, 332),
    (352, 372),
    (392, 412),
    (432, 452),
    (472, 492),
    (512, 532),
    (552, 572),
];

static DSP113_BASS_MGMT: BassManagement = BassManagement {
    global_lpf_addr: 0,
    global_hpf_addr: 20,
    gain_status_base: 40,
    gain_base: 48,
    per_channel_bpf: &DSP113_BM_BPF,
    // 9 × 8 BM matrix mixer with the LFE virtual row at row 9 (0-based 8).
    // Status idx 56..127 (= 9*8 = 72 cells), gain idx 128..199, polarity
    // idx 200..271 — verified against
    // `protocol/assets/audio_firmware/113/DSP_default_parameters.xml`. DSP 115 reuses the
    // identical layout (verified at lines 809..1024 of 115's defaults).
    bm_routing: BmMatrixMixer {
        rows: 9,
        cols: 8,
        status_base: 56,
        gain_base: 128,
        polarity_base: 200,
    },
};

static DSP113_METERS: Meters = Meters {
    input: &[784, 785, 786, 787, 788, 789, 790, 791],
    output: &[816, 817, 818, 819, 820, 821, 822, 823],
    compressor: &[808, 809, 810, 811, 812, 813, 814, 815],
    dirac_in: &[792, 793, 794, 795, 796, 797, 798, 799],
    dirac_out: &[800, 801, 802, 803, 804, 805, 806, 807],
};

pub static DSP_113: DspTable = DspTable {
    inputs: &[], // No input PEQ on old firmware
    outputs: &DSP113_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 592,
        gain_base: 656,
        polarity_base: Some(720),
        rows: 8,
        cols: 8,
    },
    meters: &DSP113_METERS,
    dirac: Some(DiracBlock {
        base_addr: 6624,
        size: 3332,
        channels: 8,
        fir_taps: 640,
    }),
    bass_management: Some(&DSP113_BASS_MGMT),
    // Flex HT / HTx old FW @ 48 kHz: 0 .. 30 ms step 0.02.
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 30.0,
        step_ms: 0.02,
    },
    // `<params addr="0" size="6624">`. Dirac block follows at idx 6624.
    param_memory_size: 6624,
    cmd_version: 3,
    // HT family has no documented convert path in `113/info.xml`.
    intermediate_dsp_id: None,
};

// ========================================================================
// DSP 115 — Flex HT/HTx new FW (8 in / 8 out, 48kHz, bass mgmt + input PEQ)
// Same output and bass management as DSP 113, plus input PEQ.
// ========================================================================

static DSP115_INPUT_PEQS: [[PeqBand; 10]; 8] = [
    peq_bands_10(1656),
    peq_bands_10(1706),
    peq_bands_10(1756),
    peq_bands_10(1806),
    peq_bands_10(1856),
    peq_bands_10(1906),
    peq_bands_10(1956),
    peq_bands_10(2006),
];

static DSP115_INPUTS: [InputChannel; 8] = [
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[0],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[1],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[2],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[3],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[4],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[5],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[6],
        fir: None,
    },
    InputChannel {
        mute: None,
        gain: None,
        polarity: None,
        peq: &DSP115_INPUT_PEQS[7],
        fir: None,
    },
];

pub static DSP_115: DspTable = DspTable {
    inputs: &DSP115_INPUTS,
    outputs: &DSP113_OUTPUTS, // Same output addresses as DSP 113
    routing: RoutingMatrix {
        status_base: 592,
        gain_base: 656,
        polarity_base: Some(720),
        rows: 8,
        cols: 8,
    },
    meters: &DSP113_METERS,
    dirac: None, // DSP 115 replaced Dirac with input PEQ
    bass_management: Some(&DSP113_BASS_MGMT),
    // Flex HT / HTx new FW @ 48 kHz: 0 .. 30 ms step 0.02.
    delay_range: DelayRange {
        min_ms: 0.0,
        max_ms: 30.0,
        step_ms: 0.02,
    },
    // `<params addr="0" size="8224">` — larger than 113 because 115 adds a
    // standalone input-PEQ block before Dirac. Dirac block follows at 8224.
    param_memory_size: 8224,
    cmd_version: 3,
    // HT family has no documented convert path in `115/info.xml`.
    intermediate_dsp_id: None,
};

// ========================================================================
// Legacy new-dialect families imported from cached SigmaStudio config.xml
// ========================================================================

static LEGACY_MSHARC_IN_PEQ: [[PeqBand; 10]; 4] = [
    peq_bands_10(16592),
    peq_bands_10(16642),
    peq_bands_10(16692),
    peq_bands_10(16742),
];

static LEGACY_MSHARC_INPUTS: [InputChannel; 4] = [
    InputChannel {
        mute: Some(0),
        gain: Some(68),
        polarity: Some(188),
        peq: &LEGACY_MSHARC_IN_PEQ[0],
        fir: None,
    },
    InputChannel {
        mute: Some(1),
        gain: Some(69),
        polarity: Some(189),
        peq: &LEGACY_MSHARC_IN_PEQ[1],
        fir: None,
    },
    InputChannel {
        mute: Some(2),
        gain: Some(70),
        polarity: Some(190),
        peq: &LEGACY_MSHARC_IN_PEQ[2],
        fir: None,
    },
    InputChannel {
        mute: Some(3),
        gain: Some(71),
        polarity: Some(191),
        peq: &LEGACY_MSHARC_IN_PEQ[3],
        fir: None,
    },
];

static LEGACY_MSHARC_OUT_PEQ: [[PeqBand; 10]; 8] = [
    peq_bands_10(16792),
    peq_bands_10(16842),
    peq_bands_10(16892),
    peq_bands_10(16942),
    peq_bands_10(16992),
    peq_bands_10(17042),
    peq_bands_10(17092),
    peq_bands_10(17142),
];

static LEGACY_MSHARC_OUTPUTS: [OutputChannel; 8] = [
    OutputChannel {
        mute: 4,
        gain: 72,
        delay: Some(160),
        polarity: Some(193),
        peq: &LEGACY_MSHARC_OUT_PEQ[0],
        fir: Some(FirFilter {
            index: 0,
            bypass: 44,
            taps_count: 200,
            data_start: 201,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 60,
            threshold: 112,
            gain: 113,
            ratio: 114,
            knee: 115,
            attack: 116,
            release: 117,
        }),
    },
    OutputChannel {
        mute: 5,
        gain: 73,
        delay: Some(161),
        polarity: Some(194),
        peq: &LEGACY_MSHARC_OUT_PEQ[1],
        fir: Some(FirFilter {
            index: 1,
            bypass: 45,
            taps_count: 2249,
            data_start: 2250,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 61,
            threshold: 118,
            gain: 119,
            ratio: 120,
            knee: 121,
            attack: 122,
            release: 123,
        }),
    },
    OutputChannel {
        mute: 6,
        gain: 74,
        delay: Some(162),
        polarity: Some(195),
        peq: &LEGACY_MSHARC_OUT_PEQ[2],
        fir: Some(FirFilter {
            index: 2,
            bypass: 46,
            taps_count: 4298,
            data_start: 4299,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 62,
            threshold: 124,
            gain: 125,
            ratio: 126,
            knee: 127,
            attack: 128,
            release: 129,
        }),
    },
    OutputChannel {
        mute: 7,
        gain: 75,
        delay: Some(163),
        polarity: Some(196),
        peq: &LEGACY_MSHARC_OUT_PEQ[3],
        fir: Some(FirFilter {
            index: 3,
            bypass: 47,
            taps_count: 6347,
            data_start: 6348,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 63,
            threshold: 130,
            gain: 131,
            ratio: 132,
            knee: 133,
            attack: 134,
            release: 135,
        }),
    },
    OutputChannel {
        mute: 8,
        gain: 76,
        delay: Some(164),
        polarity: Some(197),
        peq: &LEGACY_MSHARC_OUT_PEQ[4],
        fir: Some(FirFilter {
            index: 4,
            bypass: 48,
            taps_count: 8396,
            data_start: 8397,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 64,
            threshold: 136,
            gain: 137,
            ratio: 138,
            knee: 139,
            attack: 140,
            release: 141,
        }),
    },
    OutputChannel {
        mute: 9,
        gain: 77,
        delay: Some(165),
        polarity: Some(198),
        peq: &LEGACY_MSHARC_OUT_PEQ[5],
        fir: Some(FirFilter {
            index: 5,
            bypass: 49,
            taps_count: 10445,
            data_start: 10446,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 65,
            threshold: 142,
            gain: 143,
            ratio: 144,
            knee: 145,
            attack: 146,
            release: 147,
        }),
    },
    OutputChannel {
        mute: 10,
        gain: 78,
        delay: Some(166),
        polarity: Some(199),
        peq: &LEGACY_MSHARC_OUT_PEQ[6],
        fir: Some(FirFilter {
            index: 6,
            bypass: 50,
            taps_count: 12494,
            data_start: 12495,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 66,
            threshold: 148,
            gain: 149,
            ratio: 150,
            knee: 151,
            attack: 152,
            release: 153,
        }),
    },
    OutputChannel {
        mute: 11,
        gain: 79,
        delay: Some(167),
        polarity: None,
        peq: &LEGACY_MSHARC_OUT_PEQ[7],
        fir: Some(FirFilter {
            index: 7,
            bypass: 51,
            taps_count: 14543,
            data_start: 14544,
            max_taps: 2048,
        }),
        crossover: None,
        compressor: Some(Compressor {
            bypass: 67,
            threshold: 154,
            gain: 155,
            ratio: 156,
            knee: 157,
            attack: 158,
            release: 159,
        }),
    },
];

static LEGACY_MSHARC_METERS: Meters = Meters {
    input: &[],
    output: &[],
    compressor: &[],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_MSHARC_4X8: DspTable = DspTable {
    inputs: &LEGACY_MSHARC_INPUTS,
    outputs: &LEGACY_MSHARC_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 12,
        gain_base: 80,
        polarity_base: None,
        rows: 4,
        cols: 8,
    },
    meters: &LEGACY_MSHARC_METERS,
    dirac: None,
    bass_management: None,
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

static LEGACY_DDRC88BM_INPUTS: [InputChannel; 8] = [
    InputChannel {
        mute: Some(0),
        gain: Some(8),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(1),
        gain: Some(9),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(2),
        gain: Some(10),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(3),
        gain: Some(11),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(4),
        gain: Some(12),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(5),
        gain: Some(13),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(6),
        gain: Some(14),
        polarity: None,
        peq: &[],
        fir: None,
    },
    InputChannel {
        mute: Some(7),
        gain: Some(15),
        polarity: None,
        peq: &[],
        fir: None,
    },
];

static LEGACY_DDRC88BM_OUT_PEQ: [[PeqBand; 10]; 8] = [
    peq_bands_10(486),
    peq_bands_10(590),
    peq_bands_10(694),
    peq_bands_10(798),
    peq_bands_10(902),
    peq_bands_10(1006),
    peq_bands_10(1110),
    peq_bands_10(1214),
];

macro_rules! legacy_ddrc88bm_output {
    ($base:expr, $peq:expr, $lpf:expr, $hpf:expr) => {
        OutputChannel {
            mute: $base,
            gain: $base + 2,
            delay: Some($base + 9),
            polarity: Some($base + 13),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $lpf,
                hpf_addr: $hpf,
            }),
            compressor: Some(Compressor {
                bypass: $base + 1,
                threshold: $base + 3,
                gain: $base + 4,
                ratio: $base + 5,
                knee: $base + 6,
                attack: $base + 7,
                release: $base + 8,
            }),
        }
    };
}

static LEGACY_DDRC88BM_OUTPUTS: [OutputChannel; 8] = [
    legacy_ddrc88bm_output!(472, &LEGACY_DDRC88BM_OUT_PEQ[0], 536, 556),
    legacy_ddrc88bm_output!(576, &LEGACY_DDRC88BM_OUT_PEQ[1], 640, 660),
    legacy_ddrc88bm_output!(680, &LEGACY_DDRC88BM_OUT_PEQ[2], 744, 764),
    legacy_ddrc88bm_output!(784, &LEGACY_DDRC88BM_OUT_PEQ[3], 848, 868),
    legacy_ddrc88bm_output!(888, &LEGACY_DDRC88BM_OUT_PEQ[4], 952, 972),
    legacy_ddrc88bm_output!(992, &LEGACY_DDRC88BM_OUT_PEQ[5], 1056, 1076),
    legacy_ddrc88bm_output!(1096, &LEGACY_DDRC88BM_OUT_PEQ[6], 1160, 1180),
    legacy_ddrc88bm_output!(1200, &LEGACY_DDRC88BM_OUT_PEQ[7], 1264, 1284),
];

static LEGACY_DDRC88BM_BM_BPF: [(u16, u16); 8] = [
    (160, 170),
    (180, 190),
    (200, 210),
    (220, 230),
    (240, 250),
    (260, 270),
    (280, 290),
    (300, 310),
];

static LEGACY_DDRC88BM_BASS_MGMT: BassManagement = BassManagement {
    global_lpf_addr: 160,
    global_hpf_addr: 170,
    gain_status_base: 0,
    gain_base: 8,
    per_channel_bpf: &LEGACY_DDRC88BM_BM_BPF,
    // Legacy DDRC-88BM doesn't expose a published BM_Mixer matrix in the
    // SigmaDSP `config.xml`; populate with zeros so the field exists but
    // dimension queries return an empty matrix and clients won't try to
    // address cells. Future legacy XML reconciliation can fill this in.
    bm_routing: BmMatrixMixer {
        rows: 0,
        cols: 0,
        status_base: 0,
        gain_base: 0,
        polarity_base: 0,
    },
};

static LEGACY_DDRC88BM_METERS: Meters = Meters {
    input: &[448, 449, 450, 451, 452, 453, 454, 455],
    output: &[464, 465, 466, 467, 468, 469, 470, 471],
    compressor: &[456, 457, 458, 459, 460, 461, 462, 463],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_DDRC_88BM: DspTable = DspTable {
    inputs: &LEGACY_DDRC88BM_INPUTS,
    outputs: &LEGACY_DDRC88BM_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 16,
        gain_base: 88,
        polarity_base: None,
        rows: 8,
        cols: 8,
    },
    meters: &LEGACY_DDRC88BM_METERS,
    dirac: None,
    bass_management: Some(&LEGACY_DDRC88BM_BASS_MGMT),
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

static LEGACY_CDSP_8X12_INPUT_PEQ: [[PeqBand; 10]; 8] = [
    peq_bands_10(324),
    peq_bands_10(378),
    peq_bands_10(432),
    peq_bands_10(486),
    peq_bands_10(540),
    peq_bands_10(594),
    peq_bands_10(648),
    peq_bands_10(702),
];

static LEGACY_CDSP_8X12_INPUTS: [InputChannel; 8] = [
    InputChannel {
        mute: Some(320),
        gain: Some(321),
        polarity: Some(323),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[0],
        fir: None,
    },
    InputChannel {
        mute: Some(374),
        gain: Some(375),
        polarity: Some(377),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[1],
        fir: None,
    },
    InputChannel {
        mute: Some(428),
        gain: Some(429),
        polarity: Some(431),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[2],
        fir: None,
    },
    InputChannel {
        mute: Some(482),
        gain: Some(483),
        polarity: Some(485),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[3],
        fir: None,
    },
    InputChannel {
        mute: Some(536),
        gain: Some(537),
        polarity: Some(539),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[4],
        fir: None,
    },
    InputChannel {
        mute: Some(590),
        gain: Some(591),
        polarity: Some(593),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[5],
        fir: None,
    },
    InputChannel {
        mute: Some(644),
        gain: Some(645),
        polarity: Some(647),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[6],
        fir: None,
    },
    InputChannel {
        mute: Some(698),
        gain: Some(699),
        polarity: Some(701),
        peq: &LEGACY_CDSP_8X12_INPUT_PEQ[7],
        fir: None,
    },
];

static LEGACY_CDSP_8X12_OUT_PEQ: [[PeqBand; 10]; 12] = [
    peq_bands_10(765),
    peq_bands_10(868),
    peq_bands_10(971),
    peq_bands_10(1074),
    peq_bands_10(1177),
    peq_bands_10(1280),
    peq_bands_10(1383),
    peq_bands_10(1486),
    peq_bands_10(1589),
    peq_bands_10(1692),
    peq_bands_10(1795),
    peq_bands_10(1898),
];

macro_rules! legacy_cdsp_8x12_output {
    ($base:expr, $peq:expr, $lpf:expr, $hpf:expr) => {
        OutputChannel {
            mute: $base,
            gain: $base + 2,
            delay: Some($base + 9),
            polarity: Some($base + 12),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $lpf,
                hpf_addr: $hpf,
            }),
            compressor: Some(Compressor {
                bypass: $base + 1,
                threshold: $base + 3,
                gain: $base + 4,
                ratio: $base + 5,
                knee: $base + 6,
                attack: $base + 7,
                release: $base + 8,
            }),
        }
    };
}

static LEGACY_CDSP_8X12_OUTPUTS: [OutputChannel; 12] = [
    legacy_cdsp_8x12_output!(752, &LEGACY_CDSP_8X12_OUT_PEQ[0], 815, 835),
    legacy_cdsp_8x12_output!(855, &LEGACY_CDSP_8X12_OUT_PEQ[1], 918, 938),
    legacy_cdsp_8x12_output!(958, &LEGACY_CDSP_8X12_OUT_PEQ[2], 1021, 1041),
    legacy_cdsp_8x12_output!(1061, &LEGACY_CDSP_8X12_OUT_PEQ[3], 1124, 1144),
    legacy_cdsp_8x12_output!(1164, &LEGACY_CDSP_8X12_OUT_PEQ[4], 1227, 1247),
    legacy_cdsp_8x12_output!(1267, &LEGACY_CDSP_8X12_OUT_PEQ[5], 1330, 1350),
    legacy_cdsp_8x12_output!(1370, &LEGACY_CDSP_8X12_OUT_PEQ[6], 1433, 1453),
    legacy_cdsp_8x12_output!(1473, &LEGACY_CDSP_8X12_OUT_PEQ[7], 1536, 1556),
    legacy_cdsp_8x12_output!(1576, &LEGACY_CDSP_8X12_OUT_PEQ[8], 1639, 1659),
    legacy_cdsp_8x12_output!(1679, &LEGACY_CDSP_8X12_OUT_PEQ[9], 1742, 1762),
    legacy_cdsp_8x12_output!(1782, &LEGACY_CDSP_8X12_OUT_PEQ[10], 1845, 1865),
    legacy_cdsp_8x12_output!(1885, &LEGACY_CDSP_8X12_OUT_PEQ[11], 1948, 1968),
];

static LEGACY_CDSP_8X12_METERS: Meters = Meters {
    input: &[288, 289, 290, 291, 292, 293, 294, 295],
    output: &[308, 309, 310, 311, 312, 313, 314, 315, 316, 317, 318, 319],
    compressor: &[296, 297, 298, 299, 300, 301, 302, 303, 304, 305, 306, 307],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_CDSP_8X12_V2: DspTable = DspTable {
    inputs: &LEGACY_CDSP_8X12_INPUTS,
    outputs: &LEGACY_CDSP_8X12_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 96,
        polarity_base: Some(192),
        rows: 8,
        cols: 12,
    },
    meters: &LEGACY_CDSP_8X12_METERS,
    dirac: None,
    bass_management: None,
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

// ========================================================================
// Legacy old-dialect families imported from cached SigmaStudio config.xml
// ========================================================================

static LEGACY_10X10HD_INPUT_PEQ: [[PeqBand; 6]; 10] = [
    peq_bands_6(81),
    peq_bands_6(111),
    peq_bands_6(141),
    peq_bands_6(171),
    peq_bands_6(201),
    peq_bands_6(231),
    peq_bands_6(261),
    peq_bands_6(291),
    peq_bands_6(321),
    peq_bands_6(351),
];

static LEGACY_10X10HD_INPUTS: [InputChannel; 10] = [
    InputChannel {
        mute: Some(59),
        gain: Some(11),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[0],
        fir: None,
    },
    InputChannel {
        mute: Some(60),
        gain: Some(12),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[1],
        fir: None,
    },
    InputChannel {
        mute: Some(61),
        gain: Some(13),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[2],
        fir: None,
    },
    InputChannel {
        mute: Some(62),
        gain: Some(14),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[3],
        fir: None,
    },
    InputChannel {
        mute: Some(63),
        gain: Some(15),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[4],
        fir: None,
    },
    InputChannel {
        mute: Some(64),
        gain: Some(16),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[5],
        fir: None,
    },
    InputChannel {
        mute: Some(65),
        gain: Some(17),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[6],
        fir: None,
    },
    InputChannel {
        mute: Some(66),
        gain: Some(18),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[7],
        fir: None,
    },
    InputChannel {
        mute: Some(67),
        gain: Some(54),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[8],
        fir: None,
    },
    InputChannel {
        mute: Some(68),
        gain: Some(55),
        polarity: None,
        peq: &LEGACY_10X10HD_INPUT_PEQ[9],
        fir: None,
    },
];

static LEGACY_10X10HD_OUT_PEQ: [[PeqBand; 6]; 10] = [
    peq_bands_6(1134),
    peq_bands_6(894),
    peq_bands_6(924),
    peq_bands_6(1164),
    peq_bands_6(954),
    peq_bands_6(984),
    peq_bands_6(1014),
    peq_bands_6(1044),
    peq_bands_6(1074),
    peq_bands_6(1104),
];

macro_rules! legacy_10x10hd_output {
    ($mute:expr, $gain:expr, $polarity:expr, $peq:expr, $lpf:expr, $hpf:expr) => {
        OutputChannel {
            mute: $mute,
            gain: $gain,
            delay: None,
            polarity: Some($polarity),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $lpf,
                hpf_addr: $hpf,
            }),
            compressor: None,
        }
    };
}

static LEGACY_10X10HD_OUTPUTS: [OutputChannel; 10] = [
    legacy_10x10hd_output!(1214, 1224, 1204, &LEGACY_10X10HD_OUT_PEQ[0], 494, 514),
    legacy_10x10hd_output!(1215, 1225, 1212, &LEGACY_10X10HD_OUT_PEQ[1], 534, 554),
    legacy_10x10hd_output!(1216, 1233, 1213, &LEGACY_10X10HD_OUT_PEQ[2], 574, 594),
    legacy_10x10hd_output!(1223, 1226, 1211, &LEGACY_10X10HD_OUT_PEQ[3], 614, 634),
    legacy_10x10hd_output!(1217, 1227, 1210, &LEGACY_10X10HD_OUT_PEQ[4], 654, 674),
    legacy_10x10hd_output!(1218, 1228, 1209, &LEGACY_10X10HD_OUT_PEQ[5], 694, 714),
    legacy_10x10hd_output!(1219, 1229, 1208, &LEGACY_10X10HD_OUT_PEQ[6], 734, 754),
    legacy_10x10hd_output!(1220, 1230, 1207, &LEGACY_10X10HD_OUT_PEQ[7], 774, 794),
    legacy_10x10hd_output!(1221, 1231, 1206, &LEGACY_10X10HD_OUT_PEQ[8], 814, 834),
    legacy_10x10hd_output!(1222, 1232, 1205, &LEGACY_10X10HD_OUT_PEQ[9], 854, 874),
];

static LEGACY_10X10HD_METERS: Meters = Meters {
    input: &[],
    output: &[],
    compressor: &[],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_10X10HD: DspTable = DspTable {
    inputs: &LEGACY_10X10HD_INPUTS,
    outputs: &LEGACY_10X10HD_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 0,
        polarity_base: None,
        rows: 0,
        cols: 0,
    },
    meters: &LEGACY_10X10HD_METERS,
    dirac: None,
    bass_management: None,
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

static LEGACY_4X10HD_INPUT_PEQ: [[PeqBand; 5]; 4] = [
    peq_bands_5(134),
    peq_bands_5(109),
    peq_bands_5(59),
    peq_bands_5(84),
];

static LEGACY_4X10HD_INPUTS: [InputChannel; 4] = [
    InputChannel {
        mute: Some(53),
        gain: Some(11),
        polarity: None,
        peq: &LEGACY_4X10HD_INPUT_PEQ[0],
        fir: None,
    },
    InputChannel {
        mute: Some(54),
        gain: Some(12),
        polarity: None,
        peq: &LEGACY_4X10HD_INPUT_PEQ[1],
        fir: None,
    },
    InputChannel {
        mute: Some(55),
        gain: Some(13),
        polarity: None,
        peq: &LEGACY_4X10HD_INPUT_PEQ[2],
        fir: None,
    },
    InputChannel {
        mute: Some(56),
        gain: Some(14),
        polarity: None,
        peq: &LEGACY_4X10HD_INPUT_PEQ[3],
        fir: None,
    },
];

static LEGACY_4X10HD_OUT_PEQ: [[PeqBand; 5]; 10] = [
    peq_bands_5(809),
    peq_bands_5(609),
    peq_bands_5(634),
    peq_bands_5(834),
    peq_bands_5(659),
    peq_bands_5(684),
    peq_bands_5(709),
    peq_bands_5(734),
    peq_bands_5(759),
    peq_bands_5(784),
];

macro_rules! legacy_4x10hd_output {
    ($mute:expr, $gain:expr, $polarity:expr, $peq:expr, $lpf:expr, $hpf:expr) => {
        OutputChannel {
            mute: $mute,
            gain: $gain,
            delay: None,
            polarity: Some($polarity),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $lpf,
                hpf_addr: $hpf,
            }),
            compressor: None,
        }
    };
}

static LEGACY_4X10HD_OUTPUTS: [OutputChannel; 10] = [
    legacy_4x10hd_output!(879, 896, 869, &LEGACY_4X10HD_OUT_PEQ[0], 569, 589),
    legacy_4x10hd_output!(880, 895, 877, &LEGACY_4X10HD_OUT_PEQ[1], 529, 549),
    legacy_4x10hd_output!(881, 894, 878, &LEGACY_4X10HD_OUT_PEQ[2], 209, 229),
    legacy_4x10hd_output!(888, 889, 876, &LEGACY_4X10HD_OUT_PEQ[3], 489, 509),
    legacy_4x10hd_output!(882, 890, 875, &LEGACY_4X10HD_OUT_PEQ[4], 249, 269),
    legacy_4x10hd_output!(883, 891, 874, &LEGACY_4X10HD_OUT_PEQ[5], 289, 309),
    legacy_4x10hd_output!(884, 892, 873, &LEGACY_4X10HD_OUT_PEQ[6], 329, 349),
    legacy_4x10hd_output!(885, 893, 872, &LEGACY_4X10HD_OUT_PEQ[7], 369, 389),
    legacy_4x10hd_output!(870, 886, 867, &LEGACY_4X10HD_OUT_PEQ[8], 409, 429),
    legacy_4x10hd_output!(871, 887, 868, &LEGACY_4X10HD_OUT_PEQ[9], 449, 469),
];

static LEGACY_4X10HD_METERS: Meters = Meters {
    input: &[],
    output: &[],
    compressor: &[],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_4X10HD: DspTable = DspTable {
    inputs: &LEGACY_4X10HD_INPUTS,
    outputs: &LEGACY_4X10HD_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 0,
        polarity_base: None,
        rows: 0,
        cols: 0,
    },
    meters: &LEGACY_4X10HD_METERS,
    dirac: None,
    bass_management: None,
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

static LEGACY_M2X4_INPUT_PEQ: [[PeqBand; 6]; 2] = [peq_bands_6(12), peq_bands_6(42)];

static LEGACY_M2X4_INPUTS: [InputChannel; 2] = [
    InputChannel {
        mute: Some(7),
        gain: Some(10),
        polarity: Some(8),
        peq: &LEGACY_M2X4_INPUT_PEQ[0],
        fir: None,
    },
    InputChannel {
        mute: Some(6),
        gain: Some(11),
        polarity: Some(9),
        peq: &LEGACY_M2X4_INPUT_PEQ[1],
        fir: None,
    },
];

static LEGACY_M2X4_OUT_PEQ: [[PeqBand; 6]; 4] = [
    peq_bands_6(322),
    peq_bands_6(232),
    peq_bands_6(292),
    peq_bands_6(262),
];

macro_rules! legacy_m2x4_output {
    ($mute:expr, $gain:expr, $delay:expr, $polarity:expr, $peq:expr, $lpf:expr, $hpf:expr) => {
        OutputChannel {
            mute: $mute,
            gain: $gain,
            delay: Some($delay),
            polarity: Some($polarity),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $lpf,
                hpf_addr: $hpf,
            }),
            compressor: None,
        }
    };
}

static LEGACY_M2X4_OUTPUTS: [OutputChannel; 4] = [
    legacy_m2x4_output!(361, 357, 354, 366, &LEGACY_M2X4_OUT_PEQ[0], 112, 132),
    legacy_m2x4_output!(363, 359, 355, 364, &LEGACY_M2X4_OUT_PEQ[1], 152, 172),
    legacy_m2x4_output!(360, 358, 353, 365, &LEGACY_M2X4_OUT_PEQ[2], 192, 212),
    legacy_m2x4_output!(362, 356, 352, 367, &LEGACY_M2X4_OUT_PEQ[3], 72, 92),
];

static LEGACY_M2X4_METERS: Meters = Meters {
    input: &[],
    output: &[],
    compressor: &[],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_M2X4: DspTable = DspTable {
    inputs: &LEGACY_M2X4_INPUTS,
    outputs: &LEGACY_M2X4_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 0,
        polarity_base: None,
        rows: 0,
        cols: 0,
    },
    meters: &LEGACY_M2X4_METERS,
    dirac: None,
    bass_management: None,
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

static LEGACY_NANODIGI_INPUT_PEQ: [[PeqBand; 5]; 4] = [
    peq_bands_5(134),
    peq_bands_5(109),
    peq_bands_5(59),
    peq_bands_5(84),
];

static LEGACY_NANODIGI_INPUTS: [InputChannel; 4] = [
    InputChannel {
        mute: Some(53),
        gain: Some(46),
        polarity: None,
        peq: &LEGACY_NANODIGI_INPUT_PEQ[0],
        fir: None,
    },
    InputChannel {
        mute: Some(54),
        gain: Some(47),
        polarity: None,
        peq: &LEGACY_NANODIGI_INPUT_PEQ[1],
        fir: None,
    },
    InputChannel {
        mute: Some(55),
        gain: Some(48),
        polarity: None,
        peq: &LEGACY_NANODIGI_INPUT_PEQ[2],
        fir: None,
    },
    InputChannel {
        mute: Some(56),
        gain: Some(49),
        polarity: None,
        peq: &LEGACY_NANODIGI_INPUT_PEQ[3],
        fir: None,
    },
];

static LEGACY_NANODIGI_OUT_PEQ: [[PeqBand; 5]; 10] = [
    peq_bands_5(809),
    peq_bands_5(609),
    peq_bands_5(634),
    peq_bands_5(834),
    peq_bands_5(659),
    peq_bands_5(684),
    peq_bands_5(709),
    peq_bands_5(734),
    peq_bands_5(759),
    peq_bands_5(784),
];

macro_rules! legacy_nanodigi_output {
    ($mute:expr, $gain:expr, $delay:expr, $polarity:expr, $peq:expr, $lpf:expr, $hpf:expr) => {
        OutputChannel {
            mute: $mute,
            gain: $gain,
            delay: $delay,
            polarity: Some($polarity),
            peq: $peq,
            fir: None,
            crossover: Some(Crossover {
                lpf_addr: $lpf,
                hpf_addr: $hpf,
            }),
            compressor: None,
        }
    };
}

static LEGACY_NANODIGI_OUTPUTS: [OutputChannel; 10] = [
    legacy_nanodigi_output!(
        879,
        896,
        Some(859),
        869,
        &LEGACY_NANODIGI_OUT_PEQ[0],
        569,
        589
    ),
    legacy_nanodigi_output!(
        880,
        895,
        Some(866),
        877,
        &LEGACY_NANODIGI_OUT_PEQ[1],
        529,
        549
    ),
    legacy_nanodigi_output!(
        881,
        894,
        Some(860),
        878,
        &LEGACY_NANODIGI_OUT_PEQ[2],
        209,
        229
    ),
    legacy_nanodigi_output!(
        888,
        889,
        Some(861),
        876,
        &LEGACY_NANODIGI_OUT_PEQ[3],
        489,
        509
    ),
    legacy_nanodigi_output!(
        882,
        890,
        Some(862),
        875,
        &LEGACY_NANODIGI_OUT_PEQ[4],
        249,
        269
    ),
    legacy_nanodigi_output!(
        883,
        891,
        Some(863),
        874,
        &LEGACY_NANODIGI_OUT_PEQ[5],
        289,
        309
    ),
    legacy_nanodigi_output!(
        884,
        892,
        Some(864),
        873,
        &LEGACY_NANODIGI_OUT_PEQ[6],
        329,
        349
    ),
    legacy_nanodigi_output!(
        885,
        893,
        Some(865),
        872,
        &LEGACY_NANODIGI_OUT_PEQ[7],
        369,
        389
    ),
    legacy_nanodigi_output!(870, 886, None, 867, &LEGACY_NANODIGI_OUT_PEQ[8], 409, 429),
    legacy_nanodigi_output!(871, 887, None, 868, &LEGACY_NANODIGI_OUT_PEQ[9], 449, 469),
];

static LEGACY_NANODIGI_METERS: Meters = Meters {
    input: &[],
    output: &[],
    compressor: &[],
    dirac_in: &[],
    dirac_out: &[],
};

pub static LEGACY_NANODIGI_2X8: DspTable = DspTable {
    inputs: &LEGACY_NANODIGI_INPUTS,
    outputs: &LEGACY_NANODIGI_OUTPUTS,
    routing: RoutingMatrix {
        status_base: 0,
        gain_base: 0,
        polarity_base: None,
        rows: 0,
        cols: 0,
    },
    meters: &LEGACY_NANODIGI_METERS,
    dirac: None,
    bass_management: None,
    delay_range: DelayRange::fallback(),
    // Legacy SigmaDSP families don't come from the modern Device Console XML
    // corpus; we don't have an authoritative param_memory_size for them yet.
    // 0 means "unknown — do not use for bounds checking".
    param_memory_size: 0,
    // Legacy families predate the modern `<CMD_VERSION>` tagging in
    // Official app `info.xml` metadata. 0 = unknown.
    cmd_version: 0,
    // No documented firmware convert paths for legacy families.
    intermediate_dsp_id: None,
};

// ========================================================================
// Helper: generate 10 PEQ bands with stride 5
// ========================================================================

const fn peq_bands_10(base: u16) -> [PeqBand; 10] {
    [
        PeqBand::new(base),
        PeqBand::new(base + 5),
        PeqBand::new(base + 10),
        PeqBand::new(base + 15),
        PeqBand::new(base + 20),
        PeqBand::new(base + 25),
        PeqBand::new(base + 30),
        PeqBand::new(base + 35),
        PeqBand::new(base + 40),
        PeqBand::new(base + 45),
    ]
}

const fn peq_bands_6(base: u16) -> [PeqBand; 6] {
    [
        PeqBand::new(base),
        PeqBand::new(base + 5),
        PeqBand::new(base + 10),
        PeqBand::new(base + 15),
        PeqBand::new(base + 20),
        PeqBand::new(base + 25),
    ]
}

const fn peq_bands_5(base: u16) -> [PeqBand; 5] {
    [
        PeqBand::new(base),
        PeqBand::new(base + 5),
        PeqBand::new(base + 10),
        PeqBand::new(base + 15),
        PeqBand::new(base + 20),
    ]
}

/// Default PEQ band frequencies in Hz used by the official app for a fresh
/// 10-band PEQ chain. Doubling schedule from 20 Hz. A client reset-to-defaults
/// should seed every band at these frequencies with Q=0.7, boost=0, type=PK,
/// matching what `protocol/assets/audio_firmware/{family}/DSP_default_parameters.xml`
/// encodes in every `<filter name="PEQ_*_*">` default block.
pub const DEFAULT_PEQ_FREQS_HZ: [u32; 10] = [20, 40, 80, 160, 320, 640, 1280, 2560, 5120, 10240];

/// Official ten-band frequency labels used after a PEQ reset.
pub fn default_peq_frequencies() -> &'static [u32; 10] {
    &DEFAULT_PEQ_FREQS_HZ
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_range_clamp_handles_out_of_range_and_nan() {
        let range = DelayRange {
            min_ms: 0.0,
            max_ms: 80.0,
            step_ms: 0.01,
        };
        assert_eq!(range.clamp(-5.0), 0.0);
        assert_eq!(range.clamp(0.0), 0.0);
        assert_eq!(range.clamp(40.0), 40.0);
        assert_eq!(range.clamp(80.0), 80.0);
        assert_eq!(range.clamp(120.0), 80.0);
        assert_eq!(range.clamp(f32::NAN), 0.0);
        assert_eq!(range.clamp(f32::INFINITY), 80.0);
        assert_eq!(range.clamp(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn delay_range_contains_is_strict() {
        let range = DelayRange {
            min_ms: 0.0,
            max_ms: 30.0,
            step_ms: 0.02,
        };
        assert!(range.contains(0.0));
        assert!(range.contains(15.0));
        assert!(range.contains(30.0));
        assert!(!range.contains(-0.1));
        assert!(!range.contains(30.01));
        assert!(!range.contains(f32::NAN));
        assert!(!range.contains(f32::INFINITY));
    }

    #[test]
    fn modern_families_match_xml_derived_delay_ranges() {
        // DSP100 2x4 HD / Flex @ 96 kHz: 0..80 ms step 0.01 per
        // protocol/assets/audio_firmware/100/DSP_default_parameters.xml
        // `<range>` attrs.
        assert_eq!(DSP_100.delay_range.max_ms, 80.0);
        assert_eq!(DSP_100.delay_range.step_ms, 0.01);
        // DSP101 DDRC-24 / Flex DL @ 48 kHz: 0..30 ms step 0.02.
        assert_eq!(DSP_101.delay_range.max_ms, 30.0);
        assert_eq!(DSP_101.delay_range.step_ms, 0.02);
        // DSP105 SHD @ 96 kHz: 0..30 ms step 0.01.
        assert_eq!(DSP_105.delay_range.max_ms, 30.0);
        assert_eq!(DSP_105.delay_range.step_ms, 0.01);
        // DSP110 Flex Eight @ 96 kHz: 0..60 ms step 0.01.
        assert_eq!(DSP_110.delay_range.max_ms, 60.0);
        assert_eq!(DSP_110.delay_range.step_ms, 0.01);
        // DSP111 Flex Eight DL @ 48 kHz: 0..30 ms step 0.02.
        assert_eq!(DSP_111.delay_range.max_ms, 30.0);
        assert_eq!(DSP_111.delay_range.step_ms, 0.02);
        // DSP113/115 Flex HT/HTx @ 48 kHz: 0..30 ms step 0.02.
        assert_eq!(DSP_113.delay_range.max_ms, 30.0);
        assert_eq!(DSP_113.delay_range.step_ms, 0.02);
        assert_eq!(DSP_115.delay_range.max_ms, 30.0);
        assert_eq!(DSP_115.delay_range.step_ms, 0.02);

        // All modern families have a zero minimum.
        for table in [
            &DSP_100, &DSP_101, &DSP_105, &DSP_110, &DSP_111, &DSP_113, &DSP_115,
        ] {
            assert_eq!(table.delay_range.min_ms, 0.0);
        }
    }

    #[test]
    fn modern_families_have_xml_derived_param_memory_size() {
        // `<params addr="0" size="N">` from each family's default params XML.
        assert_eq!(DSP_100.param_memory_size, 34960);
        assert_eq!(DSP_101.param_memory_size, 1792);
        assert_eq!(DSP_105.param_memory_size, 1792);
        assert_eq!(DSP_110.param_memory_size, 20392);
        assert_eq!(DSP_111.param_memory_size, 3568);
        assert_eq!(DSP_113.param_memory_size, 6624);
        assert_eq!(DSP_115.param_memory_size, 8224);

        // For families whose Rust table carries a `DiracBlock`, the block
        // must start immediately at `param_memory_size` — the XML always
        // encodes it as `<dirac addr="{param_size}">`. DSP_115 intentionally
        // sets `dirac: None` (runtime Dirac licensing can't be inferred from
        // XML for the new HT firmware, see RE-NOTES §"Important 115 Caution")
        // so it's excluded. If we ever repopulate DiracBlock from a fresher
        // corpus, this test catches address drift.
        for table in [&DSP_101, &DSP_105, &DSP_111, &DSP_113] {
            let dirac = table
                .dirac
                .expect("Dirac-capable family must carry a DiracBlock");
            assert_eq!(
                u32::from(dirac.base_addr),
                table.param_memory_size,
                "Dirac block should start at params end for family"
            );
        }
    }

    #[test]
    fn modern_families_declare_cmd_version_3() {
        for table in [
            &DSP_100, &DSP_101, &DSP_105, &DSP_110, &DSP_111, &DSP_113, &DSP_115,
        ] {
            assert_eq!(
                table.cmd_version, 3,
                "all modern SHARC families should be CMD_VERSION 3"
            );
        }
    }

    #[test]
    fn convert_path_families_carry_intermediate_dsp_id() {
        // 2x4 HD <-> DDRC-24 path.
        assert_eq!(DSP_100.intermediate_dsp_id, Some(201));
        assert_eq!(DSP_101.intermediate_dsp_id, Some(200));
        // Flex Eight <-> Flex Eight DL path.
        assert_eq!(DSP_110.intermediate_dsp_id, Some(211));
        assert_eq!(DSP_111.intermediate_dsp_id, Some(210));
        // SHD / HT families have no published convert path.
        assert_eq!(DSP_105.intermediate_dsp_id, None);
        assert_eq!(DSP_113.intermediate_dsp_id, None);
        assert_eq!(DSP_115.intermediate_dsp_id, None);
    }

    #[test]
    fn default_peq_freqs_schedule_doubles_from_20hz() {
        assert_eq!(DEFAULT_PEQ_FREQS_HZ.len(), 10);
        assert_eq!(DEFAULT_PEQ_FREQS_HZ[0], 20);
        assert_eq!(DEFAULT_PEQ_FREQS_HZ[9], 10240);
        // Every band is 2x the previous.
        for window in DEFAULT_PEQ_FREQS_HZ.windows(2) {
            assert_eq!(window[1], window[0] * 2);
        }
    }

    #[test]
    fn ht_bm_routing_matches_xml_addresses() {
        // Verified against `protocol/assets/audio_firmware/113/` and `115/`
        // `DSP_default_parameters.xml`. Both share the identical
        // BM_Mixer layout (113 lines 159..271, 115 lines 809..1024).
        let bm = DSP_113
            .bass_management
            .expect("DSP 113 has bass management")
            .bm_routing;
        assert_eq!(bm.rows, 9, "9 rows including LFE virtual row");
        assert_eq!(bm.cols, 8, "8 output columns");
        assert_eq!(bm.cell_count(), 72);

        // Row 0 (XML: BM_Mixer_1_*) status starts at idx 56.
        assert_eq!(bm.status_addr(0, 0), Some(56));
        assert_eq!(bm.status_addr(0, 7), Some(63));
        // Row 8 (XML: BM_Mixer_9_*, the LFE row) status starts at idx 120.
        assert_eq!(bm.status_addr(8, 0), Some(120));
        assert_eq!(bm.status_addr(8, 7), Some(127));

        // Gain region starts immediately after status — idx 128.
        assert_eq!(bm.gain_addr(0, 0), Some(128));
        assert_eq!(bm.gain_addr(8, 7), Some(199));

        // Polarity region starts after gain — idx 200.
        assert_eq!(bm.polarity_addr(0, 0), Some(200));
        assert_eq!(bm.polarity_addr(8, 7), Some(271));

        // Out-of-range queries return None instead of computing junk addrs.
        assert_eq!(bm.status_addr(9, 0), None);
        assert_eq!(bm.status_addr(0, 8), None);
        assert_eq!(bm.gain_addr(15, 0), None);

        // The LFE virtual row is the last one.
        assert!(bm.is_lfe_row(8));
        assert!(!bm.is_lfe_row(0));
        assert!(!bm.is_lfe_row(7));

        // 115 must use the same matrix layout as 113 (verified empirically;
        // assert here so a future address-table edit catches the divergence).
        let bm115 = DSP_115
            .bass_management
            .expect("DSP 115 has bass management")
            .bm_routing;
        assert_eq!(bm115.rows, bm.rows);
        assert_eq!(bm115.cols, bm.cols);
        assert_eq!(bm115.status_base, bm.status_base);
        assert_eq!(bm115.gain_base, bm.gain_base);
        assert_eq!(bm115.polarity_base, bm.polarity_base);
    }

    #[test]
    fn bm_matrix_addresses_are_contiguous_and_lfe_row_marked() {
        let bm = DSP_113
            .bass_management
            .expect("DSP 113 has bass management")
            .bm_routing;
        // 9 × 8 layout including the LFE virtual row at index 8.
        assert_eq!(bm.rows, 9);
        assert_eq!(bm.cols, 8);
        assert_eq!(bm.cell_count(), 72);
        assert!(bm.is_lfe_row(8));
        assert!(!bm.is_lfe_row(0));
        // First cell of each region matches the documented base addresses
        // (status @ 56, gain @ 128, polarity @ 200).
        assert_eq!(bm.status_addr(0, 0), Some(56));
        assert_eq!(bm.gain_addr(0, 0), Some(128));
        assert_eq!(bm.polarity_addr(0, 0), Some(200));
        // Last LFE cell sits at the end of each region.
        assert_eq!(bm.status_addr(8, 7), Some(56 + 71));
        assert_eq!(bm.gain_addr(8, 7), Some(128 + 71));
        assert_eq!(bm.polarity_addr(8, 7), Some(200 + 71));
        // Out-of-range row / col returns None.
        assert_eq!(bm.status_addr(9, 0), None);
        assert_eq!(bm.gain_addr(0, 8), None);
        assert_eq!(bm.polarity_addr(9, 8), None);
    }

    #[test]
    fn only_ht_families_expose_bass_management() {
        // HT firmware variants are the only modern families that populate
        // the bass-management block. Everything else returns None.
        assert!(DSP_113.bass_management.is_some());
        assert!(DSP_115.bass_management.is_some());
        for (name, table) in [
            ("DSP_100", &DSP_100),
            ("DSP_101", &DSP_101),
            ("DSP_105", &DSP_105),
            ("DSP_110", &DSP_110),
            ("DSP_111", &DSP_111),
        ] {
            assert!(
                table.bass_management.is_none(),
                "{name} should not expose bass management"
            );
        }
    }

    #[test]
    fn legacy_families_use_conservative_fallback_range() {
        // Legacy SigmaDSP-derived tables don't have XML-derived ranges; they
        // fall back to the tight 0..30 ms step 0.02 envelope which is the
        // tightest modern family profile.
        let fb = DelayRange::fallback();
        for table in [
            &LEGACY_MSHARC_4X8,
            &LEGACY_DDRC_88BM,
            &LEGACY_CDSP_8X12_V2,
            &LEGACY_10X10HD,
            &LEGACY_4X10HD,
            &LEGACY_M2X4,
            &LEGACY_NANODIGI_2X8,
        ] {
            assert_eq!(table.delay_range.min_ms, fb.min_ms);
            assert_eq!(table.delay_range.max_ms, fb.max_ms);
            assert_eq!(table.delay_range.step_ms, fb.step_ms);
        }
    }
}
