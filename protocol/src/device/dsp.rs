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
    pub gain_status_base: u16,
    pub gain_base: u16,
    pub per_channel_bpf: &'static [(u16, u16)], // (lpf, hpf) per channel
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
        fir: None,
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
        fir: None,
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
        fir: None,
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
        fir: None,
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
    dirac: Some(DiracBlock {
        base_addr: 496,
        size: 3332,
        channels: 2,
        fir_taps: 1088,
    }),
    bass_management: None,
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
    dirac: Some(DiracBlock {
        base_addr: 496,
        size: 5636,
        channels: 2,
        fir_taps: 2176,
    }),
    bass_management: None,
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
