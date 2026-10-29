//! Shared playback contract and sweep generation.
//!
//! This module is platform-independent. It contains the `/play` request/response
//! schema plus the deterministic sweep generator used by both desktop and ESP32.

use alloc::string::String;
use core::f32::consts::PI;

#[cfg(feature = "std")]
use serde::{Deserialize, Serialize};

const DEFAULT_F1_HZ: f32 = 5.0;
const DEFAULT_F2_HZ: f32 = 400.0;
const DEFAULT_SWEEP_DURATION_SEC: f32 = 5.46;
const DEFAULT_FADE_DURATION_SEC: f32 = 0.02;
const DEFAULT_TONE_FREQUENCY_HZ: f32 = 80.0;
const DEFAULT_TONE_CYCLES: f32 = 0.0;
const DEFAULT_MARKER_CYCLES: f32 = 1.0;
const DEFAULT_MARKER_GAP_MS: u32 = 50;
const DEFAULT_TONE_FADE_OUT_SEC: f32 = 0.005;
const DEFAULT_PREAMBLE_SEC: f32 = 2.0;
const DEFAULT_GAP_SEC: f32 = 0.0;
const DEFAULT_DECAY_SEC: f32 = 2.0;
/// REW transitions from linear to logarithmic sweep at this frequency.
const LINEAR_LOG_CROSSOVER_HZ: f32 = 10.0;
const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_PREAMBLE_LEVEL_DBFS: f32 = -60.0;
pub(crate) const CHANNELS: usize = 2;
const FAST_SIN_TABLE_SIZE: usize = 512;
const SWEEP_DELTA_REANCHOR_SAMPLES: usize = 1024;
/// Upper bounds that keep every sample count and offset representable in a
/// 32-bit `usize` (ESP32): 600 s at 384 kHz is ~2^28 samples per segment.
pub(crate) const MAX_SEGMENT_SEC: f32 = 600.0;
const MAX_SAMPLE_RATE_HZ: u32 = 384_000;

include!(concat!(env!("OUT_DIR"), "/fast_sin_lut.rs"));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub enum BitDepth {
    #[cfg_attr(feature = "std", serde(rename = "16"))]
    #[default]
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
#[cfg_attr(feature = "std", serde(rename_all = "camelCase", deny_unknown_fields))]
pub struct PlayRequest {
    #[cfg_attr(feature = "std", serde(default = "default_sweep_duration_sec"))]
    pub sweep_duration_sec: f32,
    #[cfg_attr(feature = "std", serde(default = "default_f1_hz"))]
    pub sweep_start_hz: f32,
    #[cfg_attr(feature = "std", serde(default = "default_f2_hz"))]
    pub sweep_end_hz: f32,
    #[cfg_attr(feature = "std", serde(default = "default_fade_duration_sec"))]
    pub sweep_fade_sec: f32,
    #[cfg_attr(feature = "std", serde(default = "default_marker_enabled"))]
    pub marker_before: bool,
    #[cfg_attr(feature = "std", serde(default = "default_marker_enabled"))]
    pub marker_after: bool,
    #[cfg_attr(feature = "std", serde(default = "default_marker_gap_ms"))]
    pub marker_gap_ms: u32,
    #[cfg_attr(feature = "std", serde(default = "default_tone_frequency_hz"))]
    pub marker_frequency_hz: f32,
    #[cfg_attr(feature = "std", serde(default = "default_marker_cycles"))]
    pub marker_cycles: f32,
    #[cfg_attr(feature = "std", serde(default = "default_decay_sec"))]
    pub decay_sec: f32,
    #[cfg_attr(feature = "std", serde(default = "default_sample_rate_hz"))]
    pub sample_rate: u32,
    #[cfg_attr(feature = "std", serde(default))]
    pub bit_depth: BitDepth,
    #[cfg_attr(feature = "std", serde(skip))]
    pub resolved_sweep: SweepParams,
    #[cfg_attr(feature = "std", serde(skip))]
    pub resolved_tone_burst: ToneBurstParams,
    #[cfg_attr(feature = "std", serde(skip))]
    pub resolved_tail_tone_burst: Option<ToneBurstParams>,
    #[cfg_attr(feature = "std", serde(skip))]
    pub resolved_preamble_sec: f32,
    #[cfg_attr(feature = "std", serde(skip))]
    pub resolved_gap_sec: f32,
}

impl Default for PlayRequest {
    fn default() -> Self {
        Self {
            sweep_duration_sec: DEFAULT_SWEEP_DURATION_SEC,
            sweep_start_hz: DEFAULT_F1_HZ,
            sweep_end_hz: DEFAULT_F2_HZ,
            sweep_fade_sec: DEFAULT_FADE_DURATION_SEC,
            marker_before: true,
            marker_after: true,
            marker_gap_ms: default_marker_gap_ms(),
            marker_frequency_hz: DEFAULT_TONE_FREQUENCY_HZ,
            marker_cycles: DEFAULT_MARKER_CYCLES,
            decay_sec: DEFAULT_DECAY_SEC,
            sample_rate: DEFAULT_SAMPLE_RATE_HZ,
            bit_depth: BitDepth::default(),
            resolved_sweep: SweepParams::default(),
            resolved_tone_burst: ToneBurstParams::default(),
            resolved_tail_tone_burst: None,
            resolved_preamble_sec: DEFAULT_PREAMBLE_SEC,
            resolved_gap_sec: DEFAULT_GAP_SEC,
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
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub tail_tone_burst_offset_samples: Option<usize>,
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

/// `GET /devices/{id}/play`: whether playback is busy, and how the most
/// recent run that returned `200` from `POST /play` ended. `/play` answers
/// once streaming starts, so this is how a client learns that the rest of
/// the signal failed to play.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct PlaybackStatus {
    /// True from `POST /play` until the run, including any source restore,
    /// is finished. A new `/play` is rejected with `409` while true.
    pub playing: bool,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub last: Option<PlaybackRunStatus>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct PlaybackRunStatus {
    /// Same value the run's `/play` response returned.
    pub started_at_us: i64,
    pub total_samples: usize,
    pub sample_rate: u32,
    pub state: PlaybackRunState,
    /// Why the signal did not play to the end (`state == failed`).
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub error: Option<String>,
    /// The source switched for playback could not be restored.
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub source_restore_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub enum PlaybackRunState {
    Playing,
    Completed,
    Failed,
}

impl PlaybackRunStatus {
    pub fn started(response: &PlayResponse) -> Self {
        Self {
            started_at_us: response.started_at_us,
            total_samples: response.total_samples,
            sample_rate: response.sample_rate,
            state: PlaybackRunState::Playing,
            error: None,
            source_restore_error: None,
        }
    }

    /// Settle a run: completed, or failed with `error`.
    pub fn finish(&mut self, error: Option<String>) {
        self.state = if error.is_some() {
            PlaybackRunState::Failed
        } else {
            PlaybackRunState::Completed
        };
        self.error = error;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackOffsets {
    pub tone_burst_offset_samples: usize,
    pub sweep_offset_samples: usize,
    pub tail_tone_burst_offset_samples: Option<usize>,
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
    /// `ln(sweep_ratio_step)`, kept separately because `sweep_ratio_step`
    /// itself is within a few f32 ulps of 1.0 and too coarse to rebuild the
    /// phase increment from.
    pub sweep_log_step: f32,
    pub preamble_samples: usize,
    pub tone_burst_samples: usize,
    pub gap_samples: usize,
    pub sweep_samples: usize,
    pub tail_marker_gap_samples: usize,
    pub tail_tone_burst_samples: usize,
    pub decay_samples: usize,
    /// When f1 < 10 Hz, the sweep starts with a linear section from f1 to
    /// LINEAR_LOG_CROSSOVER_HZ, then switches to logarithmic. This field
    /// stores the sample index where the crossover happens (0 = pure log).
    pub linear_section_samples: usize,
    /// Start frequency of the logarithmic section (10 Hz when hybrid, f1 when pure log).
    pub log_start_hz: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct ResolvedPlayRequest {
    request: PlayRequest,
    tail_marker_gap_sec: f32,
}

impl PlaybackPlan {
    pub fn build(request: PlayRequest) -> Result<Self, PlaybackError> {
        let resolved = resolve_play_request(request)?;
        let request = resolved.request;
        validate_request(&request)?;

        let sample_rate = request.sample_rate as f32;
        let sweep_f1 = request.resolved_sweep.f1;
        let sweep_f2 = request.resolved_sweep.f2.min(sample_rate * 0.5);
        if sweep_f1 >= sweep_f2 {
            return Err(PlaybackError::InvalidSweepRange { sweep_f1, sweep_f2 });
        }

        // REW-style hybrid sweep: when f1 < 10 Hz, use a linear sweep from
        // f1 to 10 Hz, then logarithmic from 10 Hz to f2. The linear section
        // gets a proportional share of the total sweep duration based on the
        // frequency span it covers relative to the log section's octave span.
        let (linear_section_samples, log_start_hz) = if sweep_f1 < LINEAR_LOG_CROSSOVER_HZ
            && sweep_f2 > LINEAR_LOG_CROSSOVER_HZ
        {
            let linear_span = LINEAR_LOG_CROSSOVER_HZ - sweep_f1;
            let log_octaves = (sweep_f2 / LINEAR_LOG_CROSSOVER_HZ).log2();
            // Weight: 1 octave of log ≈ same perceptual time as the linear span
            let linear_weight = linear_span / LINEAR_LOG_CROSSOVER_HZ;
            let total_weight = linear_weight + log_octaves;
            let linear_fraction = linear_weight / total_weight;
            let linear_samples =
                (request.resolved_sweep.duration * linear_fraction * request.sample_rate as f32)
                    .round() as usize;
            (linear_samples, LINEAR_LOG_CROSSOVER_HZ)
        } else {
            (0, sweep_f1)
        };

        let log_duration = request.resolved_sweep.duration
            - linear_section_samples as f32 / request.sample_rate as f32;
        let raw_sweep_samples =
            seconds_to_samples(request.resolved_sweep.duration, request.sample_rate);

        let sweep_samples = find_truncated_sweep_len(
            raw_sweep_samples,
            request.sample_rate,
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
        let sweep_log_step = if log_samples > 0 {
            sweep_ln_ratio / log_samples as f32
        } else {
            0.0
        };
        let sweep_ratio_step = sweep_log_step.exp();

        Ok(Self {
            raw_sweep_samples,
            sweep_phase_scale,
            sweep_ratio_step,
            sweep_log_step,
            preamble_samples: seconds_to_samples(
                request.resolved_preamble_sec,
                request.sample_rate,
            ),
            tone_burst_samples: tone_burst_samples(
                request.resolved_tone_burst,
                request.sample_rate,
            ),
            gap_samples: seconds_to_samples(request.resolved_gap_sec, request.sample_rate),
            tail_marker_gap_samples: seconds_to_samples(
                resolved.tail_marker_gap_sec,
                request.sample_rate,
            ),
            tail_tone_burst_samples: tone_burst_samples(
                request.resolved_tail_tone_burst.unwrap_or_default(),
                request.sample_rate,
            ),
            decay_samples: seconds_to_samples(request.decay_sec, request.sample_rate),
            sweep_samples,
            sweep_f1,
            sweep_f2,
            linear_section_samples,
            log_start_hz,
            request,
        })
    }

    pub fn diagnostic_tail_marker(
        tone: ToneBurstParams,
        sample_rate: u32,
        bit_depth: BitDepth,
        decay_sec: f32,
    ) -> Result<Self, PlaybackError> {
        validate_sample_rate(sample_rate)?;
        if !decay_sec.is_finite() || decay_sec > MAX_SEGMENT_SEC {
            return Err(PlaybackError::InvalidRequest(
                "segment durations must be finite and at most 600 s",
            ));
        }
        if decay_sec < 0.0 {
            return Err(PlaybackError::InvalidRequest(
                "segment durations must be >= 0",
            ));
        }
        validate_tone_burst(
            tone,
            "markerCycles must be >= 0",
            "markerFrequencyHz must be > 0 when markerCycles > 0",
        )?;
        if tone.cycles > 0.0 && tone_burst_samples(tone, sample_rate) == 0 {
            return Err(PlaybackError::InvalidRequest(
                "marker signal is too short for the requested sampleRate",
            ));
        }

        let request = PlayRequest {
            sample_rate,
            bit_depth,
            decay_sec,
            resolved_tail_tone_burst: Some(tone),
            ..PlayRequest::default()
        };

        Ok(Self {
            request,
            sweep_f1: DEFAULT_F1_HZ,
            sweep_f2: DEFAULT_F2_HZ,
            raw_sweep_samples: 0,
            sweep_phase_scale: 0.0,
            sweep_ratio_step: 1.0,
            sweep_log_step: 0.0,
            preamble_samples: 0,
            tone_burst_samples: 0,
            gap_samples: 0,
            sweep_samples: 0,
            tail_marker_gap_samples: 0,
            tail_tone_burst_samples: tone_burst_samples(tone, sample_rate),
            decay_samples: seconds_to_samples(decay_sec, sample_rate),
            linear_section_samples: 0,
            log_start_hz: DEFAULT_F1_HZ,
        })
    }

    pub const fn offsets(&self) -> PlaybackOffsets {
        let sweep_offset_samples =
            self.preamble_samples + self.tone_burst_samples + self.gap_samples;
        let tail_tone_burst_offset_samples = if self.tail_tone_burst_samples > 0 {
            Some(sweep_offset_samples + self.sweep_samples + self.tail_marker_gap_samples)
        } else {
            None
        };
        PlaybackOffsets {
            tone_burst_offset_samples: self.preamble_samples,
            sweep_offset_samples,
            tail_tone_burst_offset_samples,
            total_samples: sweep_offset_samples
                + self.sweep_samples
                + self.tail_marker_gap_samples
                + self.tail_tone_burst_samples
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

    /// The sweep fades in over at least one cycle of its start frequency: the
    /// default 20 ms is a tenth of a 5 Hz cycle, and that abrupt an onset
    /// thumps a subwoofer. An explicit zero fade stays zero, and the fade-out
    /// keeps the requested length (the sweep ends high, where 20 ms is many
    /// cycles).
    fn sweep_fade_in_samples(&self) -> usize {
        let requested = self.request.resolved_sweep.fade_duration;
        let fade = if requested > 0.0 && self.sweep_f1 > 0.0 {
            requested.max(1.0 / self.sweep_f1)
        } else {
            requested
        };
        seconds_to_samples(fade, self.request.sample_rate).min(self.sweep_samples / 2)
    }

    fn sweep_fade_out_samples(&self) -> usize {
        seconds_to_samples(
            self.request.resolved_sweep.fade_duration,
            self.request.sample_rate,
        )
        .min(self.sweep_samples / 2)
    }

    pub fn generator(&self) -> PlaybackGenerator {
        PlaybackGenerator {
            plan: self.clone(),
            offsets: self.offsets(),
            fade_in_samples: self.sweep_fade_in_samples(),
            fade_out_samples: self.sweep_fade_out_samples(),
            cursor: 0,
            sweep_phase: 0.0,
            sweep_phase_delta: self.sweep_phase_delta_at(0),
        }
    }

    /// Phase advance from log-section sample `log_index` to the next one,
    /// in closed form. `exp_m1` keeps the increment exact for narrow sweeps,
    /// where `sweep_ratio_step - 1.0` rounds to zero in f32.
    fn sweep_phase_delta_at(&self, log_index: usize) -> f32 {
        self.sweep_phase_scale
            * self.sweep_log_step.exp_m1()
            * (self.sweep_log_step * log_index as f32).exp()
    }

    pub fn sample_at(&self, index: usize) -> f32 {
        let offsets = self.offsets();
        let tail_start = offsets
            .tail_tone_burst_offset_samples
            .unwrap_or(offsets.sweep_offset_samples + self.sweep_samples);
        if index < self.preamble_samples {
            return preamble_noise(index);
        }
        if index < self.preamble_samples + self.tone_burst_samples {
            let local = index - self.preamble_samples;
            return tone_burst_sample(
                local,
                self.request.sample_rate,
                self.request.resolved_tone_burst,
            );
        }
        if index < offsets.sweep_offset_samples {
            return 0.0;
        }
        if index < offsets.sweep_offset_samples + self.sweep_samples {
            let local = index - offsets.sweep_offset_samples;
            return sweep_sample(local, self);
        }
        if let Some(tail_offset) = offsets.tail_tone_burst_offset_samples
            && index >= tail_offset
            && index < tail_start + self.tail_tone_burst_samples
        {
            let local = index - tail_offset;
            return tone_burst_sample(
                local,
                self.request.sample_rate,
                self.request.resolved_tail_tone_burst.unwrap_or_default(),
            );
        }
        // Decay section: silence (post-sweep quiet period).
        0.0
    }
}

fn resolve_play_request(mut request: PlayRequest) -> Result<ResolvedPlayRequest, PlaybackError> {
    request.resolved_sweep = SweepParams {
        f1: request.sweep_start_hz,
        f2: request.sweep_end_hz,
        duration: request.sweep_duration_sec,
        fade_duration: request.sweep_fade_sec,
    };
    request.resolved_preamble_sec = DEFAULT_PREAMBLE_SEC;
    request.resolved_gap_sec = 0.0;
    request.resolved_tone_burst = ToneBurstParams {
        frequency: request.marker_frequency_hz,
        cycles: 0.0,
    };
    request.resolved_tail_tone_burst = None;

    if request.marker_gap_ms > 2_000 {
        return Err(PlaybackError::InvalidRequest("markerGapMs must be <= 2000"));
    }

    let marker_gap_sec = request.marker_gap_ms as f32 / 1000.0;
    let marker = ToneBurstParams {
        frequency: request.marker_frequency_hz,
        cycles: request.marker_cycles,
    };

    if request.marker_before && request.marker_cycles > 0.0 {
        request.resolved_tone_burst = marker;
        request.resolved_gap_sec = marker_gap_sec;
    }
    let tail_marker_gap_sec = if request.marker_after && request.marker_cycles > 0.0 {
        marker_gap_sec
    } else {
        0.0
    };
    request.resolved_tail_tone_burst =
        (request.marker_after && request.marker_cycles > 0.0).then_some(marker);

    Ok(ResolvedPlayRequest {
        request,
        tail_marker_gap_sec,
    })
}

#[derive(Debug, Clone)]
pub struct PlaybackGenerator {
    plan: PlaybackPlan,
    offsets: PlaybackOffsets,
    fade_in_samples: usize,
    fade_out_samples: usize,
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
        let tail_start = self
            .offsets
            .tail_tone_burst_offset_samples
            .unwrap_or(sweep_end);
        let tail_end = tail_start + self.plan.tail_tone_burst_samples;
        let sample_rate = self.plan.request.sample_rate;
        let tone = self.plan.request.resolved_tone_burst;
        let tail_tone = self
            .plan
            .request
            .resolved_tail_tone_burst
            .unwrap_or_default();
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
                    } else if index < tail_start {
                        let segment_frames = (tail_start - index).min(frames - frame_cursor);
                        out[frame_offset..frame_offset + segment_frames * frame_bytes].fill(0);
                        frame_cursor += segment_frames;
                    } else if index < tail_end {
                        let segment_frames = (tail_end - index).min(frames - frame_cursor);
                        fill_tone_pcm16(
                            index - tail_start,
                            segment_frames,
                            sample_rate,
                            tail_tone,
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
                    } else if index < tail_start {
                        let segment_frames = (tail_start - index).min(frames - frame_cursor);
                        out[frame_offset..frame_offset + segment_frames * frame_bytes].fill(0);
                        frame_cursor += segment_frames;
                    } else if index < tail_end {
                        let segment_frames = (tail_end - index).min(frames - frame_cursor);
                        fill_tone_pcm24(
                            index - tail_start,
                            segment_frames,
                            sample_rate,
                            tail_tone,
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
        let sample = if index < self.plan.linear_section_samples {
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
                self.sweep_phase_delta = self.plan.sweep_phase_delta_at(0);
            }
            let s = fast_sin_wrapped(self.sweep_phase);
            self.sweep_phase += self.sweep_phase_delta;
            if self.sweep_phase >= 2.0 * PI {
                self.sweep_phase -= 2.0 * PI;
            }
            // The per-sample ratio is rounded to an f32 ulp near 1.0, and that
            // error compounds over the sweep (a 5-400 Hz sweep ended near
            // 397 Hz). Re-anchor to the closed form periodically; between
            // anchors the recurrence drifts by well under 0.01%.
            let next_log_index = index + 1 - self.plan.linear_section_samples;
            if next_log_index.is_multiple_of(SWEEP_DELTA_REANCHOR_SAMPLES) {
                self.sweep_phase_delta = self.plan.sweep_phase_delta_at(next_log_index);
            } else {
                self.sweep_phase_delta *= self.plan.sweep_ratio_step;
            }
            s
        };
        sample
            * sweep_gain(
                index,
                self.plan.sweep_samples,
                self.fade_in_samples,
                self.fade_out_samples,
            )
    }
}

fn fill_preamble_pcm16(start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        write_pcm16_stereo(preamble_noise_pcm16(start_index + frame_idx), frame);
    }
}

fn fill_preamble_pcm24(start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.as_chunks_mut::<6>().0.iter_mut().enumerate() {
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
    for (frame_idx, frame) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        write_pcm16_stereo(
            quantize_pcm16(generator.next_sweep_sample(start_index + frame_idx)),
            frame,
        );
    }
}

fn fill_sweep_pcm24(generator: &mut PlaybackGenerator, start_index: usize, out: &mut [u8]) {
    for (frame_idx, frame) in out.as_chunks_mut::<6>().0.iter_mut().enumerate() {
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

fn default_marker_enabled() -> bool {
    true
}

fn default_marker_gap_ms() -> u32 {
    DEFAULT_MARKER_GAP_MS
}

fn default_marker_cycles() -> f32 {
    DEFAULT_MARKER_CYCLES
}

fn default_decay_sec() -> f32 {
    DEFAULT_DECAY_SEC
}

pub(crate) fn default_sample_rate_hz() -> u32 {
    DEFAULT_SAMPLE_RATE_HZ
}

fn validate_request(request: &PlayRequest) -> Result<(), PlaybackError> {
    validate_sample_rate(request.sample_rate)?;
    if !request.resolved_sweep.f1.is_finite() || !request.resolved_sweep.f2.is_finite() {
        return Err(PlaybackError::InvalidRequest(
            "frequencies and cycle counts must be finite",
        ));
    }
    let durations = [
        request.resolved_sweep.duration,
        request.resolved_sweep.fade_duration,
        request.resolved_preamble_sec,
        request.resolved_gap_sec,
        request.decay_sec,
    ];
    if durations
        .iter()
        .any(|seconds| !seconds.is_finite() || *seconds > MAX_SEGMENT_SEC)
    {
        return Err(PlaybackError::InvalidRequest(
            "segment durations must be finite and at most 600 s",
        ));
    }
    if request.resolved_sweep.f1 <= 0.0 {
        return Err(PlaybackError::InvalidRequest("sweepStartHz must be > 0"));
    }
    if request.resolved_sweep.f2 <= request.resolved_sweep.f1 {
        return Err(PlaybackError::InvalidRequest(
            "sweepEndHz must be greater than sweepStartHz",
        ));
    }
    if request.resolved_sweep.duration <= 0.0 {
        return Err(PlaybackError::InvalidRequest(
            "sweepDurationSec must be > 0",
        ));
    }
    if request.resolved_sweep.fade_duration < 0.0 {
        return Err(PlaybackError::InvalidRequest("sweepFadeSec must be >= 0"));
    }
    validate_tone_burst(
        request.resolved_tone_burst,
        "markerCycles must be >= 0",
        "markerFrequencyHz must be > 0 when markerCycles > 0",
    )?;
    if request.resolved_tone_burst.cycles > 0.0
        && tone_burst_samples(request.resolved_tone_burst, request.sample_rate) == 0
    {
        return Err(PlaybackError::InvalidRequest(
            "marker signal is too short for the requested sampleRate",
        ));
    }
    if let Some(tail_tone_burst) = request.resolved_tail_tone_burst {
        validate_tone_burst(
            tail_tone_burst,
            "markerCycles must be >= 0",
            "markerFrequencyHz must be > 0 when markerCycles > 0",
        )?;
        if tail_tone_burst.cycles > 0.0
            && tone_burst_samples(tail_tone_burst, request.sample_rate) == 0
        {
            return Err(PlaybackError::InvalidRequest(
                "marker signal is too short for the requested sampleRate",
            ));
        }
    }
    if request.resolved_preamble_sec < 0.0
        || request.resolved_gap_sec < 0.0
        || request.decay_sec < 0.0
    {
        return Err(PlaybackError::InvalidRequest(
            "segment durations must be >= 0",
        ));
    }
    Ok(())
}

pub(crate) fn validate_sample_rate(sample_rate: u32) -> Result<(), PlaybackError> {
    if sample_rate == 0 {
        return Err(PlaybackError::InvalidRequest("sampleRate must be > 0"));
    }
    if sample_rate > MAX_SAMPLE_RATE_HZ {
        return Err(PlaybackError::InvalidRequest(
            "sampleRate must be at most 384000",
        ));
    }
    Ok(())
}

pub(crate) fn seconds_to_samples(seconds: f32, sample_rate: u32) -> usize {
    (seconds * sample_rate as f32).round() as usize
}

fn validate_tone_burst(
    tone_burst: ToneBurstParams,
    cycles_error: &'static str,
    frequency_error: &'static str,
) -> Result<(), PlaybackError> {
    if !tone_burst.frequency.is_finite() || !tone_burst.cycles.is_finite() {
        return Err(PlaybackError::InvalidRequest(
            "frequencies and cycle counts must be finite",
        ));
    }
    if tone_burst.cycles < 0.0 {
        return Err(PlaybackError::InvalidRequest(cycles_error));
    }
    if tone_burst.cycles > 0.0 && tone_burst.frequency <= 0.0 {
        return Err(PlaybackError::InvalidRequest(frequency_error));
    }
    if tone_burst.cycles > 0.0 && tone_burst.cycles / tone_burst.frequency > MAX_SEGMENT_SEC {
        return Err(PlaybackError::InvalidRequest(
            "marker signal must be at most 600 s",
        ));
    }
    Ok(())
}

fn tone_burst_samples(tone_burst: ToneBurstParams, sample_rate: u32) -> usize {
    if tone_burst.cycles <= 0.0 {
        return 0;
    }
    seconds_to_samples(tone_burst.cycles / tone_burst.frequency, sample_rate)
}

fn tone_burst_fade_out_samples(tone_burst: ToneBurstParams, sample_rate: u32) -> usize {
    let total_samples = tone_burst_samples(tone_burst, sample_rate);
    if total_samples == 0 {
        return 0;
    }
    seconds_to_samples(DEFAULT_TONE_FADE_OUT_SEC, sample_rate).min(total_samples / 2)
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
    let total_samples = tone_burst_samples(tone, sample_rate);
    if index >= total_samples {
        return 0.0;
    }

    let t = index as f32 / sample_rate as f32;
    let mut sample = fast_sin(2.0 * PI * tone.frequency * t);
    let fade_out_samples = tone_burst_fade_out_samples(tone, sample_rate);
    if fade_out_samples > 0 {
        let fade_start = total_samples.saturating_sub(fade_out_samples);
        if index >= fade_start {
            let fade_index = index - fade_start;
            let fade_phase = PI * (fade_index + 1) as f32 / fade_out_samples as f32;
            sample *= 0.5 * (1.0 + fade_phase.cos());
        }
    }
    sample
}

pub(crate) fn raised_cosine_fade_in_gain(index: usize, fade_samples: usize) -> f32 {
    if fade_samples <= 1 {
        return 0.0;
    }
    let denom = (fade_samples - 1) as f32;
    0.5 * (1.0 - (PI * index as f32 / denom).cos())
}

pub(crate) fn raised_cosine_fade_out_gain(index: usize, fade_samples: usize) -> f32 {
    if fade_samples <= 1 {
        return 0.0;
    }
    let denom = (fade_samples - 1) as f32;
    0.5 * (1.0 + (PI * index as f32 / denom).cos())
}

/// Generate a sweep sample for a hybrid linear+logarithmic sweep.
/// When `linear_section_samples > 0`, samples `0..linear_section_samples` use
/// a linear frequency ramp from `sweep_f1` to `log_start_hz`, then the rest
/// uses a logarithmic sweep from `log_start_hz` to `sweep_f2`.
fn hybrid_sweep_sample(index: usize, plan: &PlaybackPlan) -> f32 {
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
    let log_samples = plan
        .raw_sweep_samples
        .saturating_sub(plan.linear_section_samples);
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

/// The sweep's fade envelope, shared by `sweep_sample` and the streaming
/// generator so both produce the same samples.
fn sweep_gain(
    index: usize,
    sweep_samples: usize,
    fade_in_samples: usize,
    fade_out_samples: usize,
) -> f32 {
    let mut gain = 1.0;
    if index < fade_in_samples {
        gain *= raised_cosine_fade_in_gain(index, fade_in_samples);
    }
    let tail_start = sweep_samples.saturating_sub(fade_out_samples);
    if fade_out_samples > 0 && index >= tail_start {
        gain *= raised_cosine_fade_out_gain(index - tail_start, fade_out_samples);
    }
    gain
}

fn sweep_sample(index: usize, plan: &PlaybackPlan) -> f32 {
    hybrid_sweep_sample(index, plan)
        * sweep_gain(
            index,
            plan.sweep_samples,
            plan.sweep_fade_in_samples(),
            plan.sweep_fade_out_samples(),
        )
}

fn find_truncated_sweep_len(
    raw_sweep_samples: usize,
    sample_rate: u32,
    sweep_f1: f32,
    sweep_f2: f32,
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
    // Phase scales with 1/sample_rate, so this must be the real rate.
    let sr = sample_rate as f32;
    let log_samples = raw_sweep_samples.saturating_sub(linear_section_samples);

    let sample_at = |index: usize| -> f32 {
        if index < linear_section_samples {
            // Linear section — always short, zero-crossing search won't reach here
            // for any reasonable sweep, but handle it for correctness.
            let t = index as f32 / sr;
            let ld = linear_section_samples as f32 / sr;
            let f_end = log_start_hz;
            fast_sin(2.0 * PI * (sweep_f1 * t + (f_end - sweep_f1) / (2.0 * ld) * t * t))
        } else {
            let li = index - linear_section_samples;
            let n = li as f32;
            let total = log_samples as f32;
            let log_dur = log_samples as f32 / sr;
            let w1 = 2.0 * PI * log_start_hz;
            let l = (sweep_f2 / log_start_hz).ln();
            let k = w1 * log_dur / l;
            let linear_end_phase = if linear_section_samples > 0 {
                let t = linear_section_samples as f32 / sr;
                let ld = linear_section_samples as f32 / sr;
                2.0 * PI * (sweep_f1 * t + (log_start_hz - sweep_f1) / (2.0 * ld) * t * t)
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
            for chunk in out.as_chunks_mut::<2>().0 {
                chunk.copy_from_slice(&bytes);
            }
        }
        BitDepth::Pcm24 => {
            let quantized = (clamped * 8_388_607.0).round() as i32;
            let bytes = quantized.to_le_bytes();
            for chunk in out.as_chunks_mut::<3>().0 {
                chunk.copy_from_slice(&bytes[..3]);
            }
        }
    }
}

pub(crate) fn quantize_pcm16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32) as i16
}

pub(crate) fn quantize_pcm24(sample: f32) -> i32 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * 8_388_607.0) as i32
}

pub(crate) fn write_pcm16_stereo(sample: i16, out: &mut [u8]) {
    let bytes = sample.to_le_bytes();
    out[0] = bytes[0];
    out[1] = bytes[1];
    out[2] = bytes[0];
    out[3] = bytes[1];
}

pub(crate) fn write_pcm24_stereo(sample: i32, out: &mut [u8]) {
    let bytes = sample.to_le_bytes();
    out[0] = bytes[0];
    out[1] = bytes[1];
    out[2] = bytes[2];
    out[3] = bytes[0];
    out[4] = bytes[1];
    out[5] = bytes[2];
}

pub(crate) fn db_to_linear(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

fn fast_sin(phase: f32) -> f32 {
    let mut wrapped = phase % (2.0 * PI);
    if wrapped < 0.0 {
        wrapped += 2.0 * PI;
    }
    fast_sin_wrapped(wrapped)
}

pub(crate) fn fast_sin_wrapped(phase: f32) -> f32 {
    let position = phase * (FAST_SIN_TABLE_SIZE as f32 / (2.0 * PI));
    let index = position as usize;
    let next = index + 1;
    let frac = position - index as f32;
    FAST_SIN_LUT[index] + (FAST_SIN_LUT[next] - FAST_SIN_LUT[index]) * frac
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_plan_matches_expected_offsets() {
        let plan = PlaybackPlan::build(PlayRequest::default()).unwrap();
        let offsets = plan.offsets();
        assert_eq!(plan.preamble_samples, 96_000); // 2s * 48kHz
        assert_eq!(plan.tone_burst_samples, 600);
        assert_eq!(plan.gap_samples, 2_400);
        assert_eq!(plan.tail_marker_gap_samples, 2_400);
        assert_eq!(plan.tail_tone_burst_samples, 600);
        assert_eq!(plan.decay_samples, 96_000); // 2s * 48kHz
        assert_eq!(offsets.tone_burst_offset_samples, 96_000);
        assert_eq!(offsets.sweep_offset_samples, 99_000);
        assert_eq!(offsets.tail_tone_burst_offset_samples, Some(363_458));
        assert_eq!(offsets.total_samples, 460_058);
        assert_eq!(plan.sweep_f1, 5.0);
        assert_eq!(plan.sweep_f2, 400.0);
        assert!(plan.linear_section_samples > 0);
        assert_eq!(plan.log_start_hz, 10.0);
    }

    #[test]
    fn requested_band_is_used_exactly() {
        let request = PlayRequest {
            sweep_start_hz: 50.0,
            sweep_end_hz: 80.0,
            sweep_duration_sec: 6.0,
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
    fn marker_controls_can_disable_both_markers_for_sweep_only() {
        let request = PlayRequest {
            marker_before: false,
            marker_after: false,
            ..PlayRequest::default()
        };
        let plan = PlaybackPlan::build(request).unwrap();
        let offsets = plan.offsets();
        assert_eq!(plan.tone_burst_samples, 0);
        assert_eq!(plan.gap_samples, 0);
        assert_eq!(plan.tail_marker_gap_samples, 0);
        assert_eq!(plan.tail_tone_burst_samples, 0);
        assert_eq!(offsets.sweep_offset_samples, 96_000);
        assert_eq!(offsets.tail_tone_burst_offset_samples, None);
        assert_eq!(offsets.total_samples, 454_058);
        assert_eq!(plan.sample_at(offsets.total_samples - 1), 0.0);
    }

    #[test]
    fn marker_cycles_zero_disables_markers_and_gaps() {
        let plan = PlaybackPlan::build(PlayRequest {
            marker_cycles: 0.0,
            ..PlayRequest::default()
        })
        .unwrap();
        let offsets = plan.offsets();

        assert_eq!(plan.tone_burst_samples, 0);
        assert_eq!(plan.gap_samples, 0);
        assert_eq!(plan.tail_marker_gap_samples, 0);
        assert_eq!(plan.tail_tone_burst_samples, 0);
        assert_eq!(offsets.sweep_offset_samples, 96_000);
        assert_eq!(offsets.tail_tone_burst_offset_samples, None);
        assert_eq!(offsets.total_samples, 454_058);
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
            sweep_start_hz: 100.0,
            sweep_end_hz: 50.0,
            ..PlayRequest::default()
        };
        let err = PlaybackPlan::build(request).unwrap_err();
        assert_eq!(
            err,
            PlaybackError::InvalidRequest("sweepEndHz must be greater than sweepStartHz")
        );
    }

    #[test]
    fn invalid_request_rejects_bad_marker_frequency() {
        let request = PlayRequest {
            marker_frequency_hz: 0.0,
            marker_cycles: 3.0,
            ..PlayRequest::default()
        };
        let err = PlaybackPlan::build(request).unwrap_err();
        assert_eq!(
            err,
            PlaybackError::InvalidRequest("markerFrequencyHz must be > 0 when markerCycles > 0")
        );
    }

    #[test]
    fn invalid_request_rejects_marker_that_rounds_to_zero_samples() {
        let request = PlayRequest {
            sample_rate: 48_000,
            marker_frequency_hz: 48_000.0,
            marker_cycles: 0.4,
            marker_after: false,
            ..PlayRequest::default()
        };
        let err = PlaybackPlan::build(request).unwrap_err();
        assert_eq!(
            err,
            PlaybackError::InvalidRequest(
                "marker signal is too short for the requested sampleRate"
            )
        );
    }

    #[test]
    fn preamble_is_low_level_noise() {
        let sample = preamble_noise(17);
        assert!(sample.abs() > 0.0);
        assert!(sample.abs() < 0.01);
    }

    #[test]
    fn fast_sine_lut_preserves_original_generated_bits() {
        for (idx, actual) in FAST_SIN_LUT.iter().enumerate() {
            let phase = (idx as f64 * 2.0 * std::f64::consts::PI) / 512.0;
            let original: f32 = format!("{:.9}", phase.sin() as f32).parse().unwrap();
            assert_eq!(actual.to_bits(), original.to_bits(), "LUT entry {idx}");
        }
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

    #[test]
    fn play_request_serializes_canonical_public_shape() {
        let default_json = serde_json::to_value(PlayRequest::default()).unwrap();
        assert!(
            (default_json
                .get("sweepDurationSec")
                .unwrap()
                .as_f64()
                .unwrap()
                - 5.46)
                .abs()
                < 1e-6
        );
        assert_eq!(default_json.get("sweepStartHz").unwrap(), &json!(5.0));
        assert_eq!(default_json.get("sweepEndHz").unwrap(), &json!(400.0));
        assert_eq!(default_json.get("markerBefore").unwrap(), true);
        assert_eq!(default_json.get("markerAfter").unwrap(), true);
        assert_eq!(default_json.get("markerGapMs").unwrap(), 50);
        assert_eq!(default_json.get("markerFrequencyHz").unwrap(), &json!(80.0));
        assert_eq!(default_json.get("markerCycles").unwrap(), &json!(1.0));
        assert!(default_json.get("tailToneBurst").is_none());
        assert!(default_json.get("toneBurst").is_none());
        assert!(default_json.get("sweep").is_none());
    }

    #[test]
    fn play_request_rejects_legacy_fields() {
        let err = serde_json::from_value::<PlayRequest>(json!({
            "tailToneBurst": { "frequency": 80.0, "cycles": 3.0 }
        }))
        .unwrap_err();
        assert!(err.to_string().contains("tailToneBurst"));
    }

    #[test]
    fn marker_controls_default_to_one_cycle_and_50ms_gap() {
        let plan = PlaybackPlan::build(PlayRequest::default()).unwrap();
        let offsets = plan.offsets();

        assert_eq!(plan.tone_burst_samples, 600);
        assert_eq!(plan.gap_samples, 2_400);
        assert_eq!(plan.tail_marker_gap_samples, 2_400);
        assert_eq!(plan.tail_tone_burst_samples, 600);
        assert_eq!(offsets.tone_burst_offset_samples, 96_000);
        assert_eq!(offsets.sweep_offset_samples, 99_000);
        assert_eq!(
            offsets.tail_tone_burst_offset_samples,
            Some(offsets.sweep_offset_samples + plan.sweep_samples + 2_400)
        );
    }

    #[test]
    fn marker_controls_can_disable_pre_marker() {
        let plan = PlaybackPlan::build(PlayRequest {
            marker_before: false,
            marker_after: true,
            ..PlayRequest::default()
        })
        .unwrap();
        let offsets = plan.offsets();

        assert_eq!(plan.tone_burst_samples, 0);
        assert_eq!(plan.gap_samples, 0);
        assert_eq!(plan.tail_marker_gap_samples, 2_400);
        assert_eq!(offsets.sweep_offset_samples, 96_000);
        assert_eq!(
            offsets.tail_tone_burst_offset_samples,
            Some(offsets.sweep_offset_samples + plan.sweep_samples + 2_400)
        );
    }

    #[test]
    fn marker_gap_ms_rejects_values_above_two_seconds() {
        let err = PlaybackPlan::build(PlayRequest {
            marker_gap_ms: 2_001,
            ..PlayRequest::default()
        })
        .unwrap_err();
        assert_eq!(
            err,
            PlaybackError::InvalidRequest("markerGapMs must be <= 2000")
        );
    }

    #[test]
    fn play_response_omits_tail_offset_when_marker_disabled() {
        let without_tail = serde_json::to_value(PlayResponse {
            started_at_us: 1,
            tone_burst_offset_samples: 2,
            sweep_offset_samples: 3,
            tail_tone_burst_offset_samples: None,
            total_samples: 4,
            sample_rate: 48_000,
            source_changed: false,
            previous_source: None,
        })
        .unwrap();
        assert!(without_tail.get("tailToneBurstOffsetSamples").is_none());

        let with_tail = serde_json::to_value(PlayResponse {
            started_at_us: 1,
            tone_burst_offset_samples: 2,
            sweep_offset_samples: 3,
            tail_tone_burst_offset_samples: Some(4),
            total_samples: 5,
            sample_rate: 48_000,
            source_changed: false,
            previous_source: None,
        })
        .unwrap();
        assert_eq!(with_tail.get("tailToneBurstOffsetSamples").unwrap(), 4);
    }

    #[test]
    fn tone_burst_fades_to_zero_at_end() {
        let tone = ToneBurstParams {
            frequency: 80.0,
            cycles: 3.0,
        };
        let sample_rate = 48_000;
        let total_samples = tone_burst_samples(tone, sample_rate);
        assert_eq!(tone_burst_fade_out_samples(tone, sample_rate), 240);
        assert_eq!(tone_burst_sample(total_samples - 1, sample_rate, tone), 0.0);
    }

    #[test]
    fn sweep_fade_reaches_zero_at_end() {
        let plan = PlaybackPlan::build(PlayRequest::default()).unwrap();
        assert_eq!(sweep_sample(plan.sweep_samples - 1, &plan), 0.0);
    }

    #[test]
    fn sweep_fade_in_spans_one_start_frequency_cycle() {
        // Default 5 Hz start: one cycle is 200 ms, ten times the 20 ms fade.
        let plan = PlaybackPlan::build(PlayRequest::default()).unwrap();
        assert_eq!(plan.sweep_fade_in_samples(), 48_000 / 5);
        let fade_end = plan.sweep_fade_in_samples();
        assert!(sweep_sample(fade_end / 10, &plan).abs() < 0.03);

        // At or above 50 Hz one cycle is <= 20 ms, so the requested fade wins.
        let plan = PlaybackPlan::build(PlayRequest {
            sweep_start_hz: 100.0,
            ..PlayRequest::default()
        })
        .unwrap();
        assert_eq!(plan.sweep_fade_in_samples(), 960);
    }

    #[test]
    fn explicit_zero_sweep_fade_stays_unfaded() {
        let plan = PlaybackPlan::build(PlayRequest {
            sweep_fade_sec: 0.0,
            ..PlayRequest::default()
        })
        .unwrap();
        assert_eq!(plan.sweep_fade_in_samples(), 0);
    }

    #[test]
    fn generator_matches_reference_sweep_sample_for_sample() {
        let requests = [
            PlayRequest::default(),
            // 40 Hz start: the fade-in is one 25 ms cycle, longer than the fade-out.
            PlayRequest {
                sweep_start_hz: 40.0,
                sweep_end_hz: 60.0,
                sweep_duration_sec: 1.0,
                ..PlayRequest::default()
            },
            // Fades longer than half the sweep are clamped to half on both paths.
            PlayRequest {
                sweep_start_hz: 100.0,
                sweep_duration_sec: 0.05,
                sweep_fade_sec: 0.04,
                ..PlayRequest::default()
            },
            PlayRequest {
                sweep_fade_sec: 0.0,
                ..PlayRequest::default()
            },
        ];
        for request in requests {
            let plan = PlaybackPlan::build(PlayRequest {
                marker_before: false,
                marker_after: false,
                ..request
            })
            .unwrap();
            let offsets = plan.offsets();
            let mut generator = plan.generator();
            let mut pcm = vec![0u8; offsets.total_samples * 4];
            generator.fill_pcm_bytes(&mut pcm);
            let mut worst = (0.0f32, 0);
            for index in 0..plan.sweep_samples {
                let at = (offsets.sweep_offset_samples + index) * 4;
                let left = i16::from_le_bytes([pcm[at], pcm[at + 1]]) as f32 / i16::MAX as f32;
                let diff = (left - sweep_sample(index, &plan)).abs();
                if diff > worst.0 {
                    worst = (diff, index);
                }
            }
            // The generator's f32 phase recurrence ends the 400 Hz default ~2
            // degrees from the closed form (0.034); an envelope mismatch is
            // far larger.
            assert!(
                worst.0 < 0.05,
                "{:?}: generator differs from the reference by {} at sweep sample {} of {}",
                plan.request.resolved_sweep,
                worst.0,
                worst.1,
                plan.sweep_samples
            );
        }
    }

    #[test]
    fn diagnostic_tail_marker_starts_at_zero_without_sweep() {
        let plan = PlaybackPlan::diagnostic_tail_marker(
            ToneBurstParams {
                frequency: 80.0,
                cycles: 3.0,
            },
            48_000,
            BitDepth::Pcm16,
            2.0,
        )
        .unwrap();
        let offsets = plan.offsets();
        assert_eq!(offsets.sweep_offset_samples, 0);
        assert_eq!(offsets.tone_burst_offset_samples, 0);
        assert_eq!(offsets.tail_tone_burst_offset_samples, Some(0));
        assert_eq!(
            offsets.total_samples,
            plan.tail_tone_burst_samples + plan.decay_samples
        );
        assert_ne!(plan.sample_at(1), 0.0);
    }

    /// Run the generator through the sweep and return the instantaneous
    /// frequency it reaches on the last log-section sample.
    fn generated_sweep_end_hz(plan: &PlaybackPlan) -> f32 {
        let mut generator = plan.generator();
        for index in 0..plan.raw_sweep_samples - 1 {
            generator.next_sweep_sample(index);
        }
        generator.sweep_phase_delta / (2.0 * PI) * plan.request.sample_rate as f32
    }

    #[test]
    fn generated_sweep_reaches_requested_end_frequency() {
        for (start, end) in [(5.0, 400.0), (20.0, 20_000.0), (100.0, 110.0)] {
            let plan = PlaybackPlan::build(PlayRequest {
                sweep_start_hz: start,
                sweep_end_hz: end,
                ..PlayRequest::default()
            })
            .unwrap();
            let end_hz = generated_sweep_end_hz(&plan);
            assert!(
                (end_hz - end).abs() / end < 1e-3,
                "{start}-{end} Hz sweep ended at {end_hz} Hz"
            );
        }
    }

    #[test]
    fn narrow_sweep_is_not_silent() {
        let plan = PlaybackPlan::build(PlayRequest {
            sweep_start_hz: 100.0,
            sweep_end_hz: 100.001,
            ..PlayRequest::default()
        })
        .unwrap();
        let end_hz = generated_sweep_end_hz(&plan);
        assert!((end_hz - 100.0).abs() < 0.01, "ended at {end_hz} Hz");
    }

    #[test]
    fn sweep_truncation_lands_on_a_zero_crossing_at_every_rate() {
        for sample_rate in [44_100, 48_000, 96_000] {
            let plan = PlaybackPlan::build(PlayRequest {
                sample_rate,
                ..PlayRequest::default()
            })
            .unwrap();
            let last = hybrid_sweep_sample(plan.sweep_samples - 1, &plan);
            let next = hybrid_sweep_sample(plan.sweep_samples, &plan);
            assert!(
                last * next <= 0.0,
                "{sample_rate} Hz sweep truncated off a zero crossing ({last}, {next})"
            );
        }
    }

    #[test]
    fn unrepresentable_durations_are_rejected() {
        for request in [
            PlayRequest {
                decay_sec: 1e20,
                ..PlayRequest::default()
            },
            PlayRequest {
                sweep_duration_sec: f32::NAN,
                ..PlayRequest::default()
            },
            PlayRequest {
                marker_frequency_hz: 1e-30,
                ..PlayRequest::default()
            },
            PlayRequest {
                sample_rate: u32::MAX,
                ..PlayRequest::default()
            },
        ] {
            assert!(
                matches!(
                    PlaybackPlan::build(request.clone()),
                    Err(PlaybackError::InvalidRequest(_))
                ),
                "accepted {request:?}"
            );
        }
        assert!(
            PlaybackPlan::diagnostic_tail_marker(
                ToneBurstParams::default(),
                48_000,
                BitDepth::Pcm16,
                1e20,
            )
            .is_err()
        );
    }

    #[test]
    fn playback_status_serializes_camel_case_contract() {
        let mut run = PlaybackRunStatus::started(&PlayResponse {
            started_at_us: 42,
            tone_burst_offset_samples: 0,
            sweep_offset_samples: 0,
            tail_tone_burst_offset_samples: None,
            total_samples: 480,
            sample_rate: 48_000,
            source_changed: false,
            previous_source: None,
        });
        let playing = serde_json::to_value(PlaybackStatus {
            playing: true,
            last: Some(run.clone()),
        })
        .unwrap();
        assert_eq!(
            playing,
            serde_json::json!({
                "playing": true,
                "last": {
                    "startedAtUs": 42,
                    "totalSamples": 480,
                    "sampleRate": 48000,
                    "state": "playing"
                }
            })
        );

        run.finish(Some("unplugged".into()));
        run.source_restore_error = Some("restore failed".into());
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            serde_json::json!({
                "startedAtUs": 42,
                "totalSamples": 480,
                "sampleRate": 48000,
                "state": "failed",
                "error": "unplugged",
                "sourceRestoreError": "restore failed"
            })
        );
        assert_eq!(
            serde_json::to_value(PlaybackStatus::default()).unwrap(),
            serde_json::json!({ "playing": false })
        );
    }
}
