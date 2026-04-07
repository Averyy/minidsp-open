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
    pub delay: u16,
    pub polarity: u16,
    pub peq: &'static [PeqBand],
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
    polarity: Some(78),
    peq: &DSP100_INPUT_PEQ_0,
    fir: None,
};

static DSP100_INPUT_1: InputChannel = InputChannel {
    mute: Some(1),
    gain: Some(27),
    polarity: Some(79),
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
        delay: 64,
        polarity: 80,
        peq: &DSP100_OUT0_PEQ,
        crossover: Some(Crossover { lpf_addr: 8580, hpf_addr: 8600 }),
        compressor: Some(Compressor {
            bypass: 22, threshold: 40, gain: 41,
            ratio: 42, knee: 43, attack: 44, release: 45,
        }),
    },
    OutputChannel {
        mute: 3,
        gain: 29,
        delay: 65,
        polarity: 81,
        peq: &DSP100_OUT1_PEQ,
        crossover: Some(Crossover { lpf_addr: 8620, hpf_addr: 8640 }),
        compressor: Some(Compressor {
            bypass: 23, threshold: 46, gain: 47,
            ratio: 48, knee: 49, attack: 50, release: 51,
        }),
    },
    OutputChannel {
        mute: 4,
        gain: 30,
        delay: 66,
        polarity: 82,
        peq: &DSP100_OUT2_PEQ,
        crossover: Some(Crossover { lpf_addr: 8660, hpf_addr: 8680 }),
        compressor: Some(Compressor {
            bypass: 24, threshold: 52, gain: 53,
            ratio: 54, knee: 55, attack: 56, release: 57,
        }),
    },
    OutputChannel {
        mute: 5,
        gain: 31,
        delay: 67,
        polarity: 83,
        peq: &DSP100_OUT3_PEQ,
        crossover: Some(Crossover { lpf_addr: 8700, hpf_addr: 8720 }),
        compressor: Some(Compressor {
            bypass: 25, threshold: 58, gain: 59,
            ratio: 60, knee: 61, attack: 62, release: 63,
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

static DSP101_OUT0_PEQ: [PeqBand; 10] = peq_bands_10(93);
static DSP101_OUT1_PEQ: [PeqBand; 10] = peq_bands_10(197);
static DSP101_OUT2_PEQ: [PeqBand; 10] = peq_bands_10(301);
static DSP101_OUT3_PEQ: [PeqBand; 10] = peq_bands_10(405);

static DSP101_OUTPUTS: [OutputChannel; 4] = [
    OutputChannel {
        mute: 80, gain: 82, delay: 89, polarity: 92,
        peq: &DSP101_OUT0_PEQ,
        crossover: Some(Crossover { lpf_addr: 143, hpf_addr: 163 }),
        compressor: Some(Compressor {
            bypass: 81, threshold: 83, gain: 84,
            ratio: 85, knee: 86, attack: 87, release: 88,
        }),
    },
    OutputChannel {
        mute: 184, gain: 186, delay: 193, polarity: 196,
        peq: &DSP101_OUT1_PEQ,
        crossover: Some(Crossover { lpf_addr: 247, hpf_addr: 267 }),
        compressor: Some(Compressor {
            bypass: 185, threshold: 187, gain: 188,
            ratio: 189, knee: 190, attack: 191, release: 192,
        }),
    },
    OutputChannel {
        mute: 288, gain: 290, delay: 297, polarity: 300,
        peq: &DSP101_OUT2_PEQ,
        crossover: Some(Crossover { lpf_addr: 351, hpf_addr: 371 }),
        compressor: Some(Compressor {
            bypass: 289, threshold: 291, gain: 292,
            ratio: 293, knee: 294, attack: 295, release: 296,
        }),
    },
    OutputChannel {
        mute: 392, gain: 394, delay: 401, polarity: 404,
        peq: &DSP101_OUT3_PEQ,
        crossover: Some(Crossover { lpf_addr: 455, hpf_addr: 475 }),
        compressor: Some(Compressor {
            bypass: 393, threshold: 395, gain: 396,
            ratio: 397, knee: 398, attack: 399, release: 400,
        }),
    },
];

static DSP101_METERS: Meters = Meters {
    input: &[],
    output: &[79, 183, 287, 391],
    compressor: &[78, 182, 286, 390],
    dirac_in: &[24, 25],
    dirac_out: &[26, 27],
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
    inputs: &[], // No input PEQ on SHD (Dirac handles input)
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
        mute: Some(66), gain: Some(68), polarity: Some(70),
        peq: &DSP110_INPUT_PEQ_0,
        fir: Some(FirFilter { bypass: 67, taps_count: 71, data_start: 72, max_taps: 2048 }),
    },
    InputChannel {
        mute: Some(2170), gain: Some(2172), polarity: Some(2174),
        peq: &DSP110_INPUT_PEQ_1,
        fir: Some(FirFilter { bypass: 2171, taps_count: 2175, data_start: 2176, max_taps: 2048 }),
    },
];

// Output channels stride 103, starting at 4274
macro_rules! dsp110_output {
    ($base:expr, $peq:expr) => {
        OutputChannel {
            mute: $base,
            gain: $base + 2,
            delay: $base + 9,
            polarity: $base + 12,
            peq: $peq,
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
    peq_bands_10(4287), peq_bands_10(4390), peq_bands_10(4493), peq_bands_10(4596),
    peq_bands_10(4699), peq_bands_10(4802), peq_bands_10(4905), peq_bands_10(5008),
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
            delay: $base + 9,
            polarity: $base + 13,
            peq: $peq,
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
    peq_bands_10(74), peq_bands_10(178), peq_bands_10(282), peq_bands_10(386),
    peq_bands_10(490), peq_bands_10(594), peq_bands_10(698), peq_bands_10(802),
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
            delay: $base + 9,
            polarity: $base + 13,
            peq: $peq,
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
    peq_bands_10(838), peq_bands_10(942), peq_bands_10(1046), peq_bands_10(1150),
    peq_bands_10(1254), peq_bands_10(1358), peq_bands_10(1462), peq_bands_10(1566),
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
    (272, 292), (312, 332), (352, 372), (392, 412),
    (432, 452), (472, 492), (512, 532), (552, 572),
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
    peq_bands_10(1656), peq_bands_10(1706), peq_bands_10(1756), peq_bands_10(1806),
    peq_bands_10(1856), peq_bands_10(1906), peq_bands_10(1956), peq_bands_10(2006),
];

static DSP115_INPUTS: [InputChannel; 8] = [
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[0], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[1], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[2], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[3], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[4], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[5], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[6], fir: None },
    InputChannel { mute: None, gain: None, polarity: None, peq: &DSP115_INPUT_PEQS[7], fir: None },
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
