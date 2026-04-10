//! Shared playback contract and sweep generation.
//!
//! This module is platform-independent. It contains the `/play` request/response
//! schema plus the deterministic sweep generator used by both desktop and ESP32.

use core::f32::consts::PI;

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

const DEFAULT_F1_HZ: f32 = 5.0;
const DEFAULT_F2_HZ: f32 = 400.0;
const DEFAULT_SWEEP_DURATION_SEC: f32 = 5.46;
const DEFAULT_FADE_DURATION_SEC: f32 = 0.02;
const DEFAULT_TONE_FREQUENCY_HZ: f32 = 80.0;
const DEFAULT_TONE_CYCLES: f32 = 0.0;
const DEFAULT_PREAMBLE_SEC: f32 = 2.0;
const DEFAULT_GAP_SEC: f32 = 0.0;
const DEFAULT_DECAY_SEC: f32 = 2.0;
/// REW transitions from linear to logarithmic sweep at this frequency.
const LINEAR_LOG_CROSSOVER_HZ: f32 = 10.0;
const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_PREAMBLE_LEVEL_DBFS: f32 = -60.0;
const CHANNELS: usize = 2;
const FAST_SIN_TABLE_SIZE: usize = 512;

include!(concat!(env!("OUT_DIR"), "/fast_sin_lut.rs"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub enum BitDepth {
    #[cfg_attr(feature = "std", serde(rename = "16"))]
    Pcm16,
    #[cfg_attr(feature = "std", serde(rename = "24"))]
    Pcm24,
}

impl BitDepth {
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Pcm16 => 2,
            Self::Pcm24 => 3,
        }
    }

    pub const fn bits(self) -> u8 {
        match self {
            Self::Pcm16 => 16,
            Self::Pcm24 => 24,
        }
    }
}

impl Default for BitDepth {
    fn default() -> Self {
        Self::Pcm16
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct SweepParams {
    #[cfg_attr(feature = "std", serde(default = "default_f1_hz"))]
    pub f1: f32,
    #[cfg_attr(feature = "std", serde(default = "default_f2_hz"))]
    pub f2: f32,
    #[cfg_attr(feature = "std", serde(default = "default_sweep_duration_sec"))]
    pub duration: f32,
    #[cfg_attr(feature = "std", serde(default = "default_fade_duration_sec"))]
    pub fade_duration: f32,
}

impl Default for SweepParams {
    fn default() -> Self {
        Self {
            f1: DEFAULT_F1_HZ,
            f2: DEFAULT_F2_HZ,
            duration: DEFAULT_SWEEP_DURATION_SEC,
            fade_duration: DEFAULT_FADE_DURATION_SEC,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct ToneBurstParams {
    #[cfg_attr(feature = "std", serde(default = "default_tone_frequency_hz"))]
    pub frequency: f32,
    #[cfg_attr(feature = "std", serde(default))]
    pub cycles: f32,
}

impl Default for ToneBurstParams {
    fn default() -> Self {
        Self {
            frequency: DEFAULT_TONE_FREQUENCY_HZ,
            cycles: DEFAULT_TONE_CYCLES,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct PlayRequest {
    #[cfg_attr(feature = "std", serde(default))]
    pub sweep: SweepParams,
    #[cfg_attr(feature = "std", serde(default))]
    pub tone_burst: ToneBurstParams,
    #[cfg_attr(feature = "std", serde(default = "default_preamble_sec"))]
    pub preamble_sec: f32,
    #[cfg_attr(feature = "std", serde(default = "default_gap_sec"))]
    pub gap_sec: f32,
    #[cfg_attr(feature = "std", serde(default = "default_decay_sec"))]
    pub decay_sec: f32,
    #[cfg_attr(feature = "std", serde(default = "default_sample_rate_hz"))]
    pub sample_rate: u32,
    #[cfg_attr(feature = "std", serde(default))]
    pub bit_depth: BitDepth,
}

impl Default for PlayRequest {
    fn default() -> Self {
        Self {
            sweep: SweepParams::default(),
            tone_burst: ToneBurstParams::default(),
            preamble_sec: DEFAULT_PREAMBLE_SEC,
            gap_sec: DEFAULT_GAP_SEC,
            decay_sec: DEFAULT_DECAY_SEC,
            sample_rate: DEFAULT_SAMPLE_RATE_HZ,
            bit_depth: BitDepth::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct PlayResponse {
    pub started_at_us: i64,
    pub tone_burst_offset_samples: usize,
    pub sweep_offset_samples: usize,
    pub total_samples: usize,
    pub sample_rate: u32,
    pub source_changed: bool,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub previous_source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct SyncRequest {
    pub client_time_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct SyncResponse {
    pub client_time_us: i64,
    pub server_time_us: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackOffsets {
    pub tone_burst_offset_samples: usize,
    pub sweep_offset_samples: usize,
    pub total_samples: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleLayout {
    pub sample_rate: u32,
    pub bit_depth: BitDepth,
    pub channels: usize,
}

impl SampleLayout {
    pub const fn frame_bytes(self) -> usize {
        self.channels * self.bit_depth.bytes_per_sample()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackPlan {
    pub request: PlayRequest,
    pub sweep_f1: f32,
    pub sweep_f2: f32,
    pub raw_sweep_samples: usize,
    pub sweep_phase_scale: f32,
    pub sweep_ratio_step: f32,
    pub preamble_samples: usize,
    pub tone_burst_samples: usize,
    pub gap_samples: usize,
    pub sweep_samples: usize,
    pub decay_samples: usize,
    /// When f1 < 10 Hz, the sweep starts with a linear section from f1 to
    /// LINEAR_LOG_CROSSOVER_HZ, then switches to logarithmic. This field
    /// stores the sample index where the crossover happens (0 = pure log).
    pub linear_section_samples: usize,
    /// Start frequency of the logarithmic section (10 Hz when hybrid, f1 when pure log).
    pub log_start_hz: f32,
}

impl PlaybackPlan {
    pub fn build(request: PlayRequest) -> Result<Self, PlaybackError> {
        validate_request(&request)?;

        let sample_rate = request.sample_rate as f32;
        let sweep_f1 = request.sweep.f1;
        let sweep_f2 = request.sweep.f2.min(sample_rate * 0.5);
        if sweep_f1 >= sweep_f2 {
            return Err(PlaybackError::InvalidSweepRange { sweep_f1, sweep_f2 });
        }

        // REW-style hybrid sweep: when f1 < 10 Hz, use a linear sweep from
        // f1 to 10 Hz, then logarithmic from 10 Hz to f2. The linear section
        // gets a proportional share of the total sweep duration based on the
        // frequency span it covers relative to the log section's octave span.
        let (linear_section_samples, log_start_hz) =
            if sweep_f1 < LINEAR_LOG_CROSSOVER_HZ && sweep_f2 > LINEAR_LOG_CROSSOVER_HZ {
                let linear_span = LINEAR_LOG_CROSSOVER_HZ - sweep_f1;
                let log_octaves = (sweep_f2 / LINEAR_LOG_CROSSOVER_HZ).log2();
                // Weight: 1 octave of log ≈ same perceptual time as the linear span
                let linear_weight = linear_span / LINEAR_LOG_CROSSOVER_HZ;
                let total_weight = linear_weight + log_octaves;
                let linear_fraction = linear_weight / total_weight;
                let linear_samples = (request.sweep.duration * linear_fraction
                    * request.sample_rate as f32)
                    .round() as usize;
                (linear_samples, LINEAR_LOG_CROSSOVER_HZ)
            } else {
                (0, sweep_f1)
            };

        let log_duration =
            request.sweep.duration - linear_section_samples as f32 / request.sample_rate as f32;
        let raw_sweep_samples = seconds_to_samples(request.sweep.duration, request.sample_rate);

        let sweep_samples = find_truncated_sweep_len(
            raw_sweep_samples,
            request.sweep.duration,
            sweep_f1,
            sweep_f2,
            linear_section_samples,
            log_start_hz,
        )?;
        let sweep_ln_ratio = (sweep_f2 / log_start_hz).ln();
        let log_samples = raw_sweep_samples.saturating_sub(linear_section_samples);
        let sweep_phase_scale = if log_samples > 0 {
            2.0 * PI * log_start_hz * log_duration / sweep_ln_ratio
        } else {
            0.0
        };
        let sweep_ratio_step = if log_samples > 0 {
            (sweep_ln_ratio / log_samples as f32).exp()
        } else {
            1.0
        };

        Ok(Self {
            raw_sweep_samples,
            sweep_phase_scale,
            sweep_ratio_step,
            preamble_samples: seconds_to_samples(request.preamble_sec, request.sample_rate),
            tone_burst_samples: tone_burst_samples(&request),
            gap_samples: seconds_to_samples(request.gap_sec, request.sample_rate),
            decay_samples: seconds_to_samples(request.decay_sec, request.sample_rate),
            sweep_samples,
            sweep_f1,
            sweep_f2,
            linear_section_samples,
            log_start_hz,
            request,
        })
    }

    pub const fn offsets(&self) -> PlaybackOffsets {
        PlaybackOffsets {
            tone_burst_offset_samples: self.preamble_samples,
            sweep_offset_samples: self.preamble_samples
                + self.tone_burst_samples
                + self.gap_samples,
            total_samples: self.preamble_samples
                + self.tone_burst_samples
                + self.gap_samples
                + self.sweep_samples
                + self.decay_samples,
        }
    }

    pub const fn layout(&self) -> SampleLayout {
        SampleLayout {
            sample_rate: self.request.sample_rate,
            bit_depth: self.request.bit_depth,
            channels: CHANNELS,
        }
    }

    pub fn generator(&self) -> PlaybackGenerator {
        PlaybackGenerator {
            plan: self.clone(),
            offsets: self.offsets(),
            fade_samples: seconds_to_samples(
                self.request.sweep.fade_duration,
                self.request.sample_rate,
            )
            .min(self.sweep_samples),
            cursor: 0,
            sweep_phase: 0.0,
            sweep_phase_delta: self.sweep_phase_scale * (self.sweep_ratio_step - 1.0),
        }
    }

    pub fn sample_at(&self, index: usize) -> f32 {
        let offsets = self.offsets();
        if index < self.preamble_samples {
            return preamble_noise(index);
        }
        if index < self.preamble_samples + self.tone_burst_samples {
            let local = index - self.preamble_samples;
            return tone_burst_sample(local, self.request.sample_rate, self.request.tone_burst);
        }
        if index < offsets.sweep_offset_samples {
            return 0.0;
        }
        if index < offsets.sweep_offset_samples + self.sweep_samples {
            let local = index - offsets.sweep_offset_samples;
            return sweep_sample(local, self);
        }
        // Decay section: silence (post-sweep quiet period).
        0.0
    }
}

#[derive(Debug, Clone)]
pub struct PlaybackGenerator {
    plan: PlaybackPlan,
    offsets: PlaybackOffsets,
    fade_samples: usize,
    cursor: usize,
    sweep_phase: f32,
    sweep_phase_delta: f32,
}

impl PlaybackGenerator {
    pub fn fill_pcm_bytes(&mut self, out: &mut [u8]) -> usize {
        let frame_bytes = self.plan.layout().frame_bytes();
        let available_frames = out.len() / frame_bytes;
        let remaining_frames = self.remaining_samples();
        let frames = available_frames.min(remaining_frames);
        let bytes_to_write = frames * frame_bytes;
        let preamble_end = self.plan.preamble_samples;
        let tone_end = preamble_end + self.plan.tone_burst_samples;
        let sweep_start = self.offsets.sweep_offset_samples;
        let sweep_end = sweep_start + self.plan.sweep_samples;
        let sample_rate = self.plan.request.sample_rate;
        let tone = self.plan.request.tone_burst;
        let mut frame_cursor = 0usize;

        match self.plan.layout().bit_depth {
            BitDepth::Pcm16 => {
                while frame_cursor < frames {
                    let index = self.cursor + frame_cursor;
                    let frame_offset = frame_cursor * frame_bytes;
                    if index < preamble_end {
                        let segment_frames = (preamble_end - index).min(frames - frame_cursor);
                        fill_preamble_pcm16(
                            index,
                            &mut out[frame_offset..frame_offset + segment_frames * frame_bytes],
                        );
                        frame_cursor += segment_frames;
                    } else if index < tone_end {
                        let segment_frames = (tone_end - index).min(frames - frame_cursor);
                        fill_tone_pcm16(
                            index - preamble_end,
                            segment_frames,
                            sample_rate,
                            tone,
                            &mut out[frame_offset..frame_offset + segment_frames * frame_bytes],
                        );
                        frame_cursor += segment_frames;
                    } else if index < sweep_start {
                        let segment_frames = (sweep_start - index).min(frames - frame_cursor);
                        out[frame_offset..frame_offset + segment_frames * frame_bytes].fill(0);
                        frame_cursor += segment_frames;
                    } else if index < sweep_end {
                        let segment_frames = (sweep_end - index).min(frames - frame_cursor);
                        fill_sweep_pcm16(
                            self,
                            index - sweep_start,
                            &mut out[frame_offset..frame_offset + segment_frames * frame_bytes],
                        );
                        frame_cursor += segment_frames;
                    } else {
                        out[frame_offset..bytes_to_write].fill(0);
                        break;
                    }
                }
            }
            BitDepth::Pcm24 => {
                while frame_cursor < frames {
                    let index = self.cursor + frame_cursor;
                    let frame_offset = frame_cursor * frame_bytes;
                    if index < preamble_end {
                        let segment_frames = (preamble_end - index).min(frames - frame_cursor);
                        fill_preamble_pcm24(
                            index,
                            &mut out[frame_offset..frame_offset + segment_frames * frame_bytes],
                        );
                        frame_cursor += segment_frames;
                    } else if index < tone_end {
                        let segment_frames = (tone_end - index).min(frames - frame_cursor);
                        fill_tone_pcm24(
                            index - preamble_end,
                            segment_frames,
                            sample_rate,
                            tone,
                            &mut out[frame_offset..frame_offset + segment_frames * frame_bytes],
                        );
                        frame_cursor += segment_frames;
                    } else if index < sweep_start {
                        let segment_frames = (sweep_start - index).min(frames - frame_cursor);
                        out[frame_offset..frame_offset + segment_frames * frame_bytes].fill(0);
                        frame_cursor += segment_frames;
                    } else if index < sweep_end {
                        let segment_frames = (sweep_end - index).min(frames - frame_cursor);
                        fill_sweep_pcm24(
                            self,
                            index - sweep_start,
                            &mut out[frame_offset..frame_offset + segment_frames * frame_bytes],
                        );
                        frame_cursor += segment_frames;
                    } else {
                        out[frame_offset..bytes_to_write].fill(0);
                        break;
                    }
                }
            }
        }

        self.cursor += frames;
        bytes_to_write
    }

    pub fn remaining_samples(&self) -> usize {
        self.plan
            .offsets()
            .total_samples
            .saturating_sub(self.cursor)
    }

    pub fn is_finished(&self) -> bool {
        self.remaining_samples() == 0
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn next_sweep_sample(&mut self, index: usize) -> f32 {
        let mut sample = if index < self.plan.linear_section_samples {
            // Linear chirp section — compute directly (no incremental state needed
            // since the linear section is short).
            let sr = self.plan.request.sample_rate as f32;
            let t = index as f32 / sr;
            let ld = self.plan.linear_section_samples as f32 / sr;
            let f_start = self.plan.sweep_f1;
            let f_end = self.plan.log_start_hz;
            let phase = 2.0 * PI * (f_start * t + (f_end - f_start) / (2.0 * ld) * t * t);
            fast_sin(phase)
        } else {
            // Logarithmic section — use incremental phase accumulator.
            // On the first log sample, initialize phase from the linear section's end.
            if index == self.plan.linear_section_samples && self.plan.linear_section_samples > 0 {
                let sr = self.plan.request.sample_rate as f32;
                let t = self.plan.linear_section_samples as f32 / sr;
                let ld = t;
                let f_start = self.plan.sweep_f1;
                let f_end = self.plan.log_start_hz;
                self.sweep_phase =
                    2.0 * PI * (f_start * t + (f_end - f_start) / (2.0 * ld) * t * t);
                self.sweep_phase %= 2.0 * PI;
                self.sweep_phase_delta =
                    self.plan.sweep_phase_scale * (self.plan.sweep_ratio_step - 1.0);
            }
            let s = fast_sin_wrapped(self.sweep_phase);
            self.sweep_phase += self.sweep_phase_delta;
            if self.sweep_phase >= 2.0 * PI {
                self.sweep_phase -= 2.0 * PI;
            }
            self.sweep_phase_delta *= self.plan.sweep_ratio_step;
            s
        };
        if self.fade_samples > 0 {
            if index < self.fade_samples {
                sample *= 0.5 * (1.0 - (PI * index as f32 / self.fade_samples as f32).cos());
            }
            let tail_start = self.plan.sweep_samples.saturating_sub(self.fade_samples);
            if index >= tail_start {
                let tail_index = index - tail_start;
                sample *= 0.5 * (1.0 + (PI * tail_index as f32 / self.fade_samples as f32).cos());
            }
        }
        sample
    }
}

fn fill_preamble_pcm16(start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.chunks_exact_mut(4).enumerate() {
        write_pcm16_stereo(preamble_noise_pcm16(start_index + frame_idx), frame);
    }
}

fn fill_preamble_pcm24(start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.chunks_exact_mut(6).enumerate() {
        write_pcm24_stereo(preamble_noise_pcm24(start_index + frame_idx), frame);
    }
}

fn fill_tone_pcm16(
    start_index: usize,
    frames: usize,
    sample_rate: u32,
    tone: ToneBurstParams,
    out: &mut [u8],
) {
    for frame_idx in 0..frames {
        let sample = quantize_pcm16(tone_burst_sample(
            start_index + frame_idx,
            sample_rate,
            tone,
        ));
        let frame_offset = frame_idx * 4;
        write_pcm16_stereo(sample, &mut out[frame_offset..frame_offset + 4]);
    }
}

fn fill_tone_pcm24(
    start_index: usize,
    frames: usize,
    sample_rate: u32,
    tone: ToneBurstParams,
    out: &mut [u8],
) {
    for frame_idx in 0..frames {
        let sample = quantize_pcm24(tone_burst_sample(
            start_index + frame_idx,
            sample_rate,
            tone,
        ));
        let frame_offset = frame_idx * 6;
        write_pcm24_stereo(sample, &mut out[frame_offset..frame_offset + 6]);
    }
}

fn fill_sweep_pcm16(generator: &mut PlaybackGenerator, start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.chunks_exact_mut(4).enumerate() {
        write_pcm16_stereo(
            quantize_pcm16(generator.next_sweep_sample(start_index + frame_idx)),
            frame,
        );
    }
}

fn fill_sweep_pcm24(generator: &mut PlaybackGenerator, start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.chunks_exact_mut(6).enumerate() {
        write_pcm24_stereo(
            quantize_pcm24(generator.next_sweep_sample(start_index + frame_idx)),
            frame,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackError {
    InvalidRequest(&'static str),
    InvalidSweepRange { sweep_f1: f32, sweep_f2: f32 },
}

impl core::fmt::Display for PlaybackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidRequest(msg) => write!(f, "{msg}"),
            Self::InvalidSweepRange { sweep_f1, sweep_f2 } => {
                write!(f, "invalid sweep range: {sweep_f1} >= {sweep_f2}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PlaybackError {}

fn default_f1_hz() -> f32 {
    DEFAULT_F1_HZ
}

fn default_f2_hz() -> f32 {
    DEFAULT_F2_HZ
}

fn default_sweep_duration_sec() -> f32 {
    DEFAULT_SWEEP_DURATION_SEC
}

fn default_fade_duration_sec() -> f32 {
    DEFAULT_FADE_DURATION_SEC
}

fn default_tone_frequency_hz() -> f32 {
    DEFAULT_TONE_FREQUENCY_HZ
}

fn default_preamble_sec() -> f32 {
    DEFAULT_PREAMBLE_SEC
}

fn default_gap_sec() -> f32 {
    DEFAULT_GAP_SEC
}

fn default_decay_sec() -> f32 {
    DEFAULT_DECAY_SEC
}

fn default_sample_rate_hz() -> u32 {
    DEFAULT_SAMPLE_RATE_HZ
}

fn validate_request(request: &PlayRequest) -> Result<(), PlaybackError> {
    if request.sweep.f1 <= 0.0 {
        return Err(PlaybackError::InvalidRequest("sweep.f1 must be > 0"));
    }
    if request.sweep.f2 <= request.sweep.f1 {
        return Err(PlaybackError::InvalidRequest(
            "sweep.f2 must be greater than sweep.f1",
        ));
    }
    if request.sweep.duration <= 0.0 {
        return Err(PlaybackError::InvalidRequest("sweep.duration must be > 0"));
    }
    if request.sweep.fade_duration < 0.0 {
        return Err(PlaybackError::InvalidRequest(
            "sweep.fadeDuration must be >= 0",
        ));
    }
    if request.tone_burst.cycles < 0.0 {
        return Err(PlaybackError::InvalidRequest(
            "toneBurst.cycles must be >= 0",
        ));
    }
    if request.tone_burst.cycles > 0.0 && request.tone_burst.frequency <= 0.0 {
        return Err(PlaybackError::InvalidRequest(
            "toneBurst.frequency must be > 0 when toneBurst.cycles > 0",
        ));
    }
    if request.sample_rate == 0 {
        return Err(PlaybackError::InvalidRequest("sampleRate must be > 0"));
    }
    if request.preamble_sec < 0.0 || request.gap_sec < 0.0 || request.decay_sec < 0.0 {
        return Err(PlaybackError::InvalidRequest(
            "segment durations must be >= 0",
        ));
    }
    Ok(())
}

fn seconds_to_samples(seconds: f32, sample_rate: u32) -> usize {
    (seconds * sample_rate as f32).round() as usize
}

fn tone_burst_samples(request: &PlayRequest) -> usize {
    if request.tone_burst.cycles <= 0.0 {
        return 0;
    }
    seconds_to_samples(
        request.tone_burst.cycles / request.tone_burst.frequency,
        request.sample_rate,
    )
}

fn preamble_noise(index: usize) -> f32 {
    let mut state = (index as u32).wrapping_add(0x9E37_79B9);
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    let normalized = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
    normalized * db_to_linear(DEFAULT_PREAMBLE_LEVEL_DBFS)
}

fn preamble_noise_pcm16(index: usize) -> i16 {
    let mut state = (index as u32).wrapping_add(0x9E37_79B9);
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    (((state >> 24) & 0x3f) as i16) - 32
}

fn preamble_noise_pcm24(index: usize) -> i32 {
    let mut state = (index as u32).wrapping_add(0x9E37_79B9);
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    (((state >> 8) & 0x3fff) as i32) - 8192
}

fn tone_burst_sample(index: usize, sample_rate: u32, tone: ToneBurstParams) -> f32 {
    let t = index as f32 / sample_rate as f32;
    fast_sin(2.0 * PI * tone.frequency * t)
}

/// Generate a sweep sample for a hybrid linear+logarithmic sweep.
/// When `linear_section_samples > 0`, samples `0..linear_section_samples` use
/// a linear frequency ramp from `sweep_f1` to `log_start_hz`, then the rest
/// uses a logarithmic sweep from `log_start_hz` to `sweep_f2`.
fn hybrid_sweep_sample(
    index: usize,
    plan: &PlaybackPlan,
) -> f32 {
    let sr = plan.request.sample_rate as f32;

    if index < plan.linear_section_samples {
        // Linear sweep: frequency ramps linearly from f1 to log_start_hz.
        let t = index as f32 / sr;
        let linear_duration = plan.linear_section_samples as f32 / sr;
        let f_start = plan.sweep_f1;
        let f_end = plan.log_start_hz;
        // Instantaneous phase for a linear chirp: 2π * (f0*t + (f1-f0)/(2T) * t²)
        let phase = 2.0 * PI * (f_start * t + (f_end - f_start) / (2.0 * linear_duration) * t * t);
        return fast_sin(phase);
    }

    // Logarithmic sweep section
    let log_index = index - plan.linear_section_samples;
    let log_samples = plan.raw_sweep_samples.saturating_sub(plan.linear_section_samples);
    if log_samples == 0 {
        return 0.0;
    }
    let log_duration = log_samples as f32 / sr;
    let n = log_index as f32;
    let total = log_samples as f32;
    let w1 = 2.0 * PI * plan.log_start_hz;
    let l = (plan.sweep_f2 / plan.log_start_hz).ln();
    let k = w1 * log_duration / l;
    // Add phase offset so the log section is continuous with the linear section's
    // ending phase (linear ends at frequency = log_start_hz).
    let linear_end_phase = if plan.linear_section_samples > 0 {
        let t = plan.linear_section_samples as f32 / sr;
        let ld = plan.linear_section_samples as f32 / sr;
        let f_start = plan.sweep_f1;
        let f_end = plan.log_start_hz;
        2.0 * PI * (f_start * t + (f_end - f_start) / (2.0 * ld) * t * t)
    } else {
        0.0
    };
    fast_sin(linear_end_phase + k * (((n / total) * l).exp() - 1.0))
}

fn sweep_sample(index: usize, plan: &PlaybackPlan) -> f32 {
    let mut sample = hybrid_sweep_sample(index, plan);
    let fade_samples =
        seconds_to_samples(plan.request.sweep.fade_duration, plan.request.sample_rate)
            .min(plan.sweep_samples / 2);
    if fade_samples > 0 {
        // Fade-in
        if index < fade_samples {
            sample *= 0.5 * (1.0 - (PI * index as f32 / fade_samples as f32).cos());
        }
        // Fade-out
        let tail_start = plan.sweep_samples.saturating_sub(fade_samples);
        if index >= tail_start {
            let tail_index = index - tail_start;
            sample *= 0.5 * (1.0 + (PI * tail_index as f32 / fade_samples as f32).cos());
        }
    }
    sample
}

fn find_truncated_sweep_len(
    raw_sweep_samples: usize,
    _sweep_duration_sec: f32,
    _sweep_f1: f32,
    _sweep_f2: f32,
    linear_section_samples: usize,
    log_start_hz: f32,
) -> Result<usize, PlaybackError> {
    if raw_sweep_samples == 0 {
        return Err(PlaybackError::InvalidRequest(
            "sweep.duration produced zero samples",
        ));
    }

    // Build a temporary plan-like struct just for zero-crossing search.
    // We search backwards from the end to find the last zero crossing so
    // the sweep ends cleanly.
    let sr = 48_000.0f32; // Only used for relative phase — sample rate cancels out.
    let log_samples = raw_sweep_samples.saturating_sub(linear_section_samples);

    let sample_at = |index: usize| -> f32 {
        if index < linear_section_samples {
            // Linear section — always short, zero-crossing search won't reach here
            // for any reasonable sweep, but handle it for correctness.
            let t = index as f32 / sr;
            let ld = linear_section_samples as f32 / sr;
            let f_end = log_start_hz;
            fast_sin(2.0 * PI * (_sweep_f1 * t + (f_end - _sweep_f1) / (2.0 * ld) * t * t))
        } else {
            let li = index - linear_section_samples;
            let n = li as f32;
            let total = log_samples as f32;
            let log_dur = log_samples as f32 / sr;
            let w1 = 2.0 * PI * log_start_hz;
            let l = (_sweep_f2 / log_start_hz).ln();
            let k = w1 * log_dur / l;
            let linear_end_phase = if linear_section_samples > 0 {
                let t = linear_section_samples as f32 / sr;
                let ld = linear_section_samples as f32 / sr;
                2.0 * PI * (_sweep_f1 * t + (log_start_hz - _sweep_f1) / (2.0 * ld) * t * t)
            } else {
                0.0
            };
            fast_sin(linear_end_phase + k * (((n / total) * l).exp() - 1.0))
        }
    };

    let mut next = sample_at(raw_sweep_samples - 1);
    let mut last_zero = raw_sweep_samples - 1;

    for index in (0..raw_sweep_samples - 1).rev() {
        let current = sample_at(index);
        if current * next <= 0.0 {
            last_zero = index;
            break;
        }
        next = current;
    }

    Ok(last_zero + 1)
}

#[cfg(test)]
fn encode_frame(sample: f32, layout: SampleLayout, out: &mut [u8]) {
    let clamped = sample.clamp(-1.0, 1.0);
    match layout.bit_depth {
        BitDepth::Pcm16 => {
            let quantized = (clamped * i16::MAX as f32).round() as i16;
            let bytes = quantized.to_le_bytes();
            for chunk in out.chunks_exact_mut(2) {
                chunk.copy_from_slice(&bytes);
            }
        }
        BitDepth::Pcm24 => {
            let quantized = (clamped * 8_388_607.0).round() as i32;
            let bytes = quantized.to_le_bytes();
            for chunk in out.chunks_exact_mut(3) {
                chunk.copy_from_slice(&bytes[..3]);
            }
        }
    }
}

fn quantize_pcm16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32) as i16
}

fn quantize_pcm24(sample: f32) -> i32 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * 8_388_607.0) as i32
}

fn write_pcm16_stereo(sample: i16, out: &mut [u8]) {
    let bytes = sample.to_le_bytes();
    out[0] = bytes[0];
    out[1] = bytes[1];
    out[2] = bytes[0];
    out[3] = bytes[1];
}

fn write_pcm24_stereo(sample: i32, out: &mut [u8]) {
    let bytes = sample.to_le_bytes();
    out[0] = bytes[0];
    out[1] = bytes[1];
    out[2] = bytes[2];
    out[3] = bytes[0];
    out[4] = bytes[1];
    out[5] = bytes[2];
}

fn db_to_linear(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

fn fast_sin(phase: f32) -> f32 {
    let mut wrapped = phase % (2.0 * PI);
    if wrapped < 0.0 {
        wrapped += 2.0 * PI;
    }
    fast_sin_wrapped(wrapped)
}

fn fast_sin_wrapped(phase: f32) -> f32 {
    let position = phase * (FAST_SIN_TABLE_SIZE as f32 / (2.0 * PI));
    let index = position as usize;
    let next = index + 1;
    let frac = position - index as f32;
    FAST_SIN_LUT[index] + (FAST_SIN_LUT[next] - FAST_SIN_LUT[index]) * frac
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_plan_matches_expected_offsets() {
        let plan = PlaybackPlan::build(PlayRequest::default()).unwrap();
        let offsets = plan.offsets();
        // 2s preamble, 0.5s decay — matching REW's pre/post silence
        assert_eq!(plan.preamble_samples, 96_000); // 2s * 48kHz
        assert_eq!(plan.tone_burst_samples, 0);
        assert_eq!(plan.gap_samples, 0);
        assert_eq!(plan.decay_samples, 96_000); // 2s * 48kHz
        assert_eq!(offsets.tone_burst_offset_samples, 96_000);
        assert_eq!(offsets.sweep_offset_samples, 96_000);
        assert_eq!(plan.sweep_f1, 5.0);
        assert_eq!(plan.sweep_f2, 400.0);
        // Should have a linear section since f1 < 10 Hz
        assert!(plan.linear_section_samples > 0);
        assert_eq!(plan.log_start_hz, 10.0);
    }

    #[test]
    fn requested_band_is_used_exactly() {
        let request = PlayRequest {
            sweep: SweepParams {
                f1: 50.0,
                f2: 80.0,
                duration: 6.0,
                fade_duration: 0.02,
            },
            ..PlayRequest::default()
        };
        let plan = PlaybackPlan::build(request).unwrap();
        assert_eq!(plan.sweep_f1, 50.0);
        assert_eq!(plan.sweep_f2, 80.0);
    }

    #[test]
    fn generator_streams_full_signal_without_overrun() {
        let plan = PlaybackPlan::build(PlayRequest::default()).unwrap();
        let mut generator = plan.generator();
        let frame_bytes = plan.layout().frame_bytes();
        let mut buf = vec![0u8; frame_bytes * 137];
        let mut total_frames = 0usize;

        while !generator.is_finished() {
            let written = generator.fill_pcm_bytes(&mut buf);
            assert_eq!(written % frame_bytes, 0);
            total_frames += written / frame_bytes;
        }

        assert_eq!(total_frames, plan.offsets().total_samples);
        assert_eq!(generator.fill_pcm_bytes(&mut buf), 0);
    }

    #[test]
    fn pcm24_encoding_is_little_endian_and_stereo() {
        let layout = SampleLayout {
            sample_rate: 48_000,
            bit_depth: BitDepth::Pcm24,
            channels: 2,
        };
        let mut out = [0u8; 6];
        encode_frame(1.0, layout, &mut out);
        assert_eq!(out, [0xff, 0xff, 0x7f, 0xff, 0xff, 0x7f]);
    }

    #[test]
    fn invalid_request_rejects_bad_sweep() {
        let request = PlayRequest {
            sweep: SweepParams {
                f1: 100.0,
                f2: 50.0,
                ..SweepParams::default()
            },
            ..PlayRequest::default()
        };
        let err = PlaybackPlan::build(request).unwrap_err();
        assert_eq!(
            err,
            PlaybackError::InvalidRequest("sweep.f2 must be greater than sweep.f1")
        );
    }

    #[test]
    fn preamble_is_low_level_noise() {
        let sample = preamble_noise(17);
        assert!(sample.abs() > 0.0);
        assert!(sample.abs() < 0.01);
    }

    #[test]
    fn fast_sine_approximation_stays_close_to_stdlib() {
        for step in 0..4096usize {
            let phase = (step as f32 / 4096.0) * 64.0 * PI;
            let expected = phase.sin();
            let actual = fast_sin(phase);
            assert!(
                (expected - actual).abs() < 0.0002,
                "phase={phase} expected={expected} actual={actual}"
            );
        }
    }
}
