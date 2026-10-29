//! Shared rattle-finder schedule and streaming stereo PCM generator.

use alloc::{format, string::String, vec, vec::Vec};
use core::f32::consts::TAU;

use crate::playback::{
    BitDepth, CHANNELS, MAX_SEGMENT_SEC, PlaybackError, SampleLayout, db_to_linear,
    default_sample_rate_hz, fast_sin_wrapped, quantize_pcm16, quantize_pcm24,
    raised_cosine_fade_in_gain, raised_cosine_fade_out_gain, seconds_to_samples,
    validate_sample_rate, write_pcm16_stereo, write_pcm24_stereo,
};

const DEFAULT_FREQUENCIES_HZ: [f32; 11] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0,
];
const LEAD_IN_SEC: f32 = 0.25;
const MAX_SESSION_SEC: f64 = 3600.0;
const MIN_TONE_SEC: f32 = 0.5;
const MAX_TONES: u64 = 64;
const MAX_STEPS: u64 = 4096;
const PHASE_REANCHOR_SAMPLES: usize = 1024;

/// Omitted frequency fields select the ISO 1/3-octave centres, 20–200 Hz.
/// Explicit lists and the complete start/end/count form are mutually exclusive.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(
    feature = "std",
    serde(default, rename_all = "camelCase", deny_unknown_fields)
)]
pub struct RattleRequest {
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub frequencies_hz: Option<Vec<f32>>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub start_hz: Option<f32>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub end_hz: Option<f32>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub count: Option<u32>,
    pub tone_sec: f32,
    pub gap_sec: f32,
    pub cycles: u32,
    pub volumes_db: Vec<f32>,
    pub sample_rate: u32,
    pub bit_depth: BitDepth,
}

impl Default for RattleRequest {
    fn default() -> Self {
        Self {
            frequencies_hz: None,
            start_hz: None,
            end_hz: None,
            count: None,
            tone_sec: 4.0,
            gap_sec: 1.0,
            cycles: 1,
            volumes_db: vec![-35.0],
            sample_rate: default_sample_rate_hz(),
            bit_depth: BitDepth::default(),
        }
    }
}

/// Compact schedule shared by HTTP responses and clients deriving tone labels.
/// Offsets count stereo frames; the final step includes its gap.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct RattleSchedule {
    pub sample_rate: u32,
    pub lead_in_samples: usize,
    pub tone_samples: usize,
    pub gap_samples: usize,
    pub total_samples: usize,
    pub frequencies_hz: Vec<f32>,
    /// As played: quietest first, including repeated entries.
    pub volumes_db: Vec<f32>,
    pub cycles: u32,
    pub master_volume_db: f32,
}

impl RattleSchedule {
    pub fn steps(&self) -> usize {
        self.frequencies_hz
            .len()
            .saturating_mul(self.volumes_db.len())
            .saturating_mul(self.cycles as usize)
    }

    /// Derive a zero-based step without storing or transmitting a step list.
    pub fn step(&self, index: usize) -> Option<RattleStep> {
        let tone = self.tone(index)?;
        let pass = index / self.frequencies_hz.len();
        let volume_db = self.volumes_db[pass % self.volumes_db.len()];
        let frequency_hz = tone.frequency_hz;
        let label_volume_db = if volume_db == 0.0 { 0.0 } else { volume_db };
        Some(RattleStep {
            index,
            cycle: (pass / self.volumes_db.len()) as u32,
            frequency_hz,
            volume_db,
            label: format!("{frequency_hz} Hz @ {label_volume_db} dB"),
            start_sample: tone.start_sample,
            end_sample: tone.end_sample,
            gap_end_sample: tone.gap_end_sample,
            digital_gain: tone.digital_gain,
            fade_samples: tone.fade_samples,
        })
    }

    // The renderer needs numeric parameters only, without allocating a label.
    fn tone(&self, index: usize) -> Option<RattleTone> {
        if index >= self.steps() || self.sample_rate == 0 || self.tone_samples < 2 {
            return None;
        }
        let pass = index / self.frequencies_hz.len();
        let frequency_hz = self.frequencies_hz[index % self.frequencies_hz.len()];
        let volume_db = self.volumes_db[pass % self.volumes_db.len()];
        let stride = self.tone_samples.checked_add(self.gap_samples)?;
        let start_sample = self
            .lead_in_samples
            .checked_add(index.checked_mul(stride)?)?;
        let end_sample = start_sample.checked_add(self.tone_samples)?;
        Some(RattleTone {
            frequency_hz,
            start_sample,
            end_sample,
            gap_end_sample: end_sample.checked_add(self.gap_samples)?,
            digital_gain: db_to_linear(volume_db - self.master_volume_db),
            fade_samples: seconds_to_samples(0.1f32.max(2.0 / frequency_hz), self.sample_rate)
                .max(1)
                .min(self.tone_samples / 2),
        })
    }

    /// During lead-in, labels the upcoming first step; a gap labels its tone.
    /// Remaining time includes `sample_index`. Returns None at/past the end.
    pub fn current_at(&self, sample_index: usize) -> Option<RattleCurrent> {
        if sample_index >= self.total_samples {
            return None;
        }
        let stride = self.tone_samples.checked_add(self.gap_samples)?;
        let index = sample_index
            .saturating_sub(self.lead_in_samples)
            .checked_div(stride)?;
        let step = self.step(index)?;
        let (phase, remaining) = if sample_index < self.lead_in_samples {
            (
                RattleCurrentPhase::LeadIn,
                self.lead_in_samples - sample_index,
            )
        } else if sample_index < step.end_sample {
            (RattleCurrentPhase::Tone, step.end_sample - sample_index)
        } else {
            (RattleCurrentPhase::Gap, step.gap_end_sample - sample_index)
        };
        let rate = f64::from(self.sample_rate);
        Some(RattleCurrent {
            step: index,
            steps: self.steps(),
            label: step.label,
            frequency_hz: step.frequency_hz,
            volume_db: step.volume_db,
            cycle: step.cycle,
            cycles: self.cycles,
            phase,
            remaining_sec: remaining as f64 / rate,
            elapsed_sec: sample_index as f64 / rate,
            total_sec: self.total_samples as f64 / rate,
        })
    }
}

/// `200` from `POST /devices/{id}/rattle`: the stream has started.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct RattleResponse {
    pub started_at_us: i64,
    pub schedule: RattleSchedule,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub previous_volume_db: Option<f32>,
    pub source_changed: bool,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub previous_source: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct RattleCurrent {
    /// Zero-based step and cycle indices.
    pub step: usize,
    pub steps: usize,
    pub label: String,
    pub frequency_hz: f32,
    pub volume_db: f32,
    pub cycle: u32,
    pub cycles: u32,
    pub phase: RattleCurrentPhase,
    pub remaining_sec: f64,
    pub elapsed_sec: f64,
    pub total_sec: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub enum RattleCurrentPhase {
    LeadIn,
    Tone,
    Gap,
}

/// `GET /devices/{id}/rattle`: the most recent run that returned `200`.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct RattleStatus {
    /// Busy until streaming and volume/source restoration have finished.
    pub playing: bool,
    /// Filled only while this run is playing; absent once its audio ends.
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub current: Option<RattleCurrent>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub last: Option<RattleRunStatus>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct RattleRunStatus {
    pub started_at_us: i64,
    pub state: RattleRunState,
    pub schedule: RattleSchedule,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub error: Option<String>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub volume_restore_error: Option<String>,
    #[cfg_attr(feature = "std", serde(skip_serializing_if = "Option::is_none"))]
    pub source_restore_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub enum RattleRunState {
    Playing,
    Completed,
    Stopped,
    Failed,
}

impl RattleRunStatus {
    pub fn started(response: &RattleResponse) -> Self {
        Self {
            started_at_us: response.started_at_us,
            state: RattleRunState::Playing,
            schedule: response.schedule.clone(),
            error: None,
            volume_restore_error: None,
            source_restore_error: None,
        }
    }

    /// Settle the stream independently of any volume/source restore errors.
    pub fn finish(&mut self, error: Option<String>) {
        self.state = if error.is_some() {
            RattleRunState::Failed
        } else {
            RattleRunState::Completed
        };
        self.error = error;
    }

    pub fn stopped(&mut self) {
        self.state = RattleRunState::Stopped;
        self.error = None;
    }

    pub fn set_restore_errors(&mut self, volume: Option<String>, source: Option<String>) {
        self.volume_restore_error = volume;
        self.source_restore_error = source;
    }
}

/// Clock-mapped audio cursor after output latency, rounded down to a frame.
/// Negative elapsed time clamps to zero; unrepresentable results saturate.
pub fn elapsed_samples(
    started_at_us: i64,
    now_us: i64,
    sample_rate: u32,
    output_latency_us: i64,
) -> usize {
    let elapsed_us =
        (i128::from(now_us) - i128::from(started_at_us) - i128::from(output_latency_us)).max(0);
    let samples = elapsed_us * i128::from(sample_rate) / 1_000_000;
    usize::try_from(samples).unwrap_or(usize::MAX)
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "std", derive(serde::Serialize))]
#[cfg_attr(feature = "std", serde(rename_all = "camelCase"))]
pub struct RattleStep {
    /// Zero-based step and cycle indices.
    pub index: usize,
    pub cycle: u32,
    pub frequency_hz: f32,
    pub volume_db: f32,
    pub label: String,
    pub start_sample: usize,
    /// Exclusive end of the tone; the gap starts here.
    pub end_sample: usize,
    /// Exclusive end of the gap, including the final step's gap.
    pub gap_end_sample: usize,
    pub digital_gain: f32,
    pub fade_samples: usize,
}

/// Remaining samples include the sample passed to `step_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RattlePhase {
    Tone { remaining_samples: usize },
    Gap { remaining_samples: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RattlePlan {
    schedule: RattleSchedule,
    total_samples: usize,
    layout: SampleLayout,
    master_volume_db: f32,
}

impl RattlePlan {
    pub fn build(mut request: RattleRequest) -> Result<Self, PlaybackError> {
        use PlaybackError::InvalidRequest;
        validate_sample_rate(request.sample_rate)?;
        if !request.tone_sec.is_finite()
            || request.tone_sec < MIN_TONE_SEC
            || request.tone_sec > MAX_SEGMENT_SEC
            || !request.gap_sec.is_finite()
            || request.gap_sec < 0.0
            || request.gap_sec > MAX_SEGMENT_SEC
        {
            return Err(InvalidRequest(
                "toneSec must be >= 0.5 and gapSec >= 0; both finite and at most 600 s",
            ));
        }
        if request.cycles == 0 {
            return Err(InvalidRequest("cycles must be >= 1"));
        }
        if request.volumes_db.is_empty()
            || request
                .volumes_db
                .iter()
                .any(|v| !v.is_finite() || !(-127.0..=0.0).contains(v))
        {
            return Err(InvalidRequest(
                "volumesDb must be non-empty, finite and within -127..=0",
            ));
        }
        if request.volumes_db.iter().any(|v| (v * 2.0).fract() != 0.0) {
            return Err(InvalidRequest("volumesDb must use multiples of 0.5 dB"));
        }
        let has_range =
            request.start_hz.is_some() || request.end_hz.is_some() || request.count.is_some();
        if request.frequencies_hz.is_some() && has_range {
            return Err(InvalidRequest("use frequenciesHz OR startHz/endHz/count"));
        }
        let range = if has_range {
            match (request.start_hz, request.end_hz, request.count) {
                (Some(start), Some(end), Some(count)) if count > 0 && end >= start => {
                    validate_frequency(start, request.sample_rate)?;
                    validate_frequency(end, request.sample_rate)?;
                    Some((start, end, count))
                }
                _ => {
                    return Err(InvalidRequest(
                        "startHz/endHz/count are required together; count >= 1 and endHz >= startHz",
                    ));
                }
            }
        } else {
            None
        };
        let frequency_count = range.map_or_else(
            || {
                request
                    .frequencies_hz
                    .as_ref()
                    .map_or(DEFAULT_FREQUENCIES_HZ.len(), Vec::len) as u64
            },
            |(_, _, count)| u64::from(count),
        );
        if frequency_count == 0 {
            return Err(InvalidRequest("frequenciesHz must not be empty"));
        }
        // Bound schedule dimensions independently of duration, before generating
        // the frequency list (including on 32-bit).
        if frequency_count > MAX_TONES {
            return Err(InvalidRequest(
                "frequenciesHz and count are limited to 64 tones",
            ));
        }
        let step_count = frequency_count
            .checked_mul(request.volumes_db.len() as u64)
            .and_then(|n| n.checked_mul(u64::from(request.cycles)))
            .filter(|n| *n <= MAX_STEPS)
            .ok_or(InvalidRequest(
                "tones * volumes * cycles must be at most 4096",
            ))?;
        let duration = LEAD_IN_SEC as f64
            + step_count as f64 * (request.tone_sec as f64 + request.gap_sec as f64);
        if duration > MAX_SESSION_SEC {
            return Err(InvalidRequest("session must be at most 3600 s"));
        }
        let lead_in = seconds_to_samples(LEAD_IN_SEC, request.sample_rate);
        let tone_samples = seconds_to_samples(request.tone_sec, request.sample_rate);
        let gap_samples = seconds_to_samples(request.gap_sec, request.sample_rate);
        // Both edge samples must be zero, even for a very short tone.
        if tone_samples < 2 {
            return Err(InvalidRequest("toneSec must span at least two samples"));
        }
        let total_samples = step_count
            .checked_mul((tone_samples + gap_samples) as u64)
            .and_then(|n| n.checked_add(lead_in as u64))
            .filter(|n| *n <= (MAX_SESSION_SEC * request.sample_rate as f64) as u64)
            .ok_or(InvalidRequest("rounded session must be at most 3600 s"))?;
        // At the shared maximum rate, one hour is 1,382,400,000 frames.
        let total_samples = usize::try_from(total_samples)
            .map_err(|_| InvalidRequest("session sample offsets are not representable"))?;
        let frequencies: Vec<f32> = if let Some((start, end, count)) = range {
            (0..count)
                .map(|i| {
                    if i == 0 {
                        start
                    } else if i == count - 1 {
                        end
                    } else {
                        start * ((end / start).ln() * i as f32 / (count - 1) as f32).exp()
                    }
                })
                .map(|hz| (hz * 10.0).round() / 10.0)
                .collect()
        } else {
            request
                .frequencies_hz
                .unwrap_or_else(|| DEFAULT_FREQUENCIES_HZ.to_vec())
        };
        if range.is_some() && frequencies.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(InvalidRequest(
                "startHz/endHz/count must produce unique tones after rounding to 0.1 Hz",
            ));
        }
        for &frequency in &frequencies {
            validate_frequency(frequency, request.sample_rate)?;
        }
        request.volumes_db.sort_by(|a, b| a.total_cmp(b));
        let master_volume_db = *request.volumes_db.last().unwrap();
        Ok(Self {
            schedule: RattleSchedule {
                sample_rate: request.sample_rate,
                lead_in_samples: lead_in,
                tone_samples,
                gap_samples,
                total_samples,
                frequencies_hz: frequencies,
                volumes_db: request.volumes_db,
                cycles: request.cycles,
                master_volume_db,
            },
            total_samples,
            master_volume_db,
            layout: SampleLayout {
                sample_rate: request.sample_rate,
                bit_depth: request.bit_depth,
                channels: CHANNELS,
            },
        })
    }

    pub fn step_count(&self) -> usize {
        self.schedule.steps()
    }
    pub fn step(&self, index: usize) -> Option<RattleStep> {
        self.schedule.step(index)
    }
    /// Lazily derive labelled steps; no step list is retained by the plan.
    pub fn steps(&self) -> impl ExactSizeIterator<Item = RattleStep> + '_ {
        (0..self.step_count()).map(|index| self.step(index).expect("validated rattle step"))
    }
    pub fn schedule(&self) -> RattleSchedule {
        self.schedule.clone()
    }
    pub fn current_at(&self, sample_index: usize) -> Option<RattleCurrent> {
        self.schedule.current_at(sample_index)
    }
    pub const fn total_samples(&self) -> usize {
        self.total_samples
    }
    pub const fn sample_rate(&self) -> u32 {
        self.layout.sample_rate
    }
    pub const fn layout(&self) -> SampleLayout {
        self.layout
    }
    pub const fn master_volume_db(&self) -> f32 {
        self.master_volume_db
    }

    /// Returns None in the lead-in and at/past the end of the session.
    pub fn step_at(&self, sample_index: usize) -> Option<(RattleStep, RattlePhase)> {
        let (index, phase) = self.step_index_at(sample_index)?;
        Some((self.step(index)?, phase))
    }

    /// Locate a step without computing tone parameters or allocating a label.
    /// Returns None in the lead-in and at/past the end of the session.
    pub fn step_index_at(&self, sample_index: usize) -> Option<(usize, RattlePhase)> {
        if sample_index < self.schedule.lead_in_samples || sample_index >= self.total_samples {
            return None;
        }
        let stride = self.schedule.tone_samples + self.schedule.gap_samples;
        let offset = sample_index - self.schedule.lead_in_samples;
        let local = offset % stride;
        let phase = if local < self.schedule.tone_samples {
            RattlePhase::Tone {
                remaining_samples: self.schedule.tone_samples - local,
            }
        } else {
            RattlePhase::Gap {
                remaining_samples: stride - local,
            }
        };
        Some((offset / stride, phase))
    }

    pub fn generator(&self) -> RattleGenerator {
        RattleGenerator {
            plan: self.clone(),
            cursor: 0,
            step_index: 0,
            tone: self.schedule.tone(0).expect("validated first rattle tone"),
            phase: 0.0,
            phase_delta: 0.0,
            last_tone_gain: 0.0,
            stop: None,
            stopped: false,
        }
    }
}

fn validate_frequency(frequency: f32, sample_rate: u32) -> Result<(), PlaybackError> {
    if !frequency.is_finite()
        || !(5.0..=2000.0).contains(&frequency)
        || frequency >= sample_rate as f32 * 0.5
    {
        return Err(PlaybackError::InvalidRequest(
            "frequencies must be finite, within 5..=2000 Hz and below Nyquist",
        ));
    }
    Ok(())
}

fn tone_gain(start: usize, end: usize, fade: usize, index: usize) -> f32 {
    let local = index - start;
    if local < fade {
        raised_cosine_fade_in_gain(local, fade)
    } else if index >= end - fade {
        raised_cosine_fade_out_gain(index - (end - fade), fade)
    } else {
        1.0
    }
}

/// Only the current tone's rendering parameters are cached, without a label.
#[derive(Debug, Clone, Copy)]
struct RattleTone {
    frequency_hz: f32,
    start_sample: usize,
    end_sample: usize,
    gap_end_sample: usize,
    digital_gain: f32,
    fade_samples: usize,
}

#[derive(Debug, Clone, Copy)]
struct StopFade {
    start: usize,
    end: usize,
    gain: f32,
}

#[derive(Debug, Clone)]
pub struct RattleGenerator {
    plan: RattlePlan,
    cursor: usize,
    step_index: usize,
    tone: RattleTone,
    phase: f32,
    phase_delta: f32,
    last_tone_gain: f32,
    stop: Option<StopFade>,
    stopped: bool,
}

impl RattleGenerator {
    /// Writes complete frames only; leaves any unused suffix untouched.
    pub fn fill_pcm_bytes(&mut self, out: &mut [u8]) -> usize {
        let frame_bytes = self.plan.layout.frame_bytes();
        let end = self.stop.map_or(self.plan.total_samples, |stop| stop.end);
        let frames = (out.len() / frame_bytes).min(end.saturating_sub(self.cursor));
        for frame in out[..frames * frame_bytes].chunks_exact_mut(frame_bytes) {
            if self.cursor >= self.tone.gap_end_sample {
                self.step_index += 1;
                self.tone = self
                    .plan
                    .schedule
                    .tone(self.step_index)
                    .expect("validated next rattle tone");
            }
            let step = &self.tone;
            let sample = if self.cursor >= step.start_sample && self.cursor < step.end_sample {
                let local = self.cursor - step.start_sample;
                if local == 0 {
                    self.phase_delta = TAU * step.frequency_hz / self.plan.sample_rate() as f32;
                }
                if local.is_multiple_of(PHASE_REANCHOR_SAMPLES) {
                    // Re-anchor from the exact tone sample index. Keep f64 out
                    // of the per-sample recurrence on the ESP32-S3.
                    let cycles =
                        step.frequency_hz as f64 * local as f64 / self.plan.sample_rate() as f64;
                    self.phase = ((cycles % 1.0) * core::f64::consts::TAU) as f32;
                    // Rounding to f32 can turn a phase just below TAU into TAU.
                    if self.phase >= TAU {
                        self.phase = 0.0;
                    }
                }
                let gain = if let Some(stop) = self.stop {
                    stop.gain
                        * raised_cosine_fade_out_gain(
                            self.cursor - stop.start,
                            stop.end - stop.start,
                        )
                } else {
                    tone_gain(
                        step.start_sample,
                        step.end_sample,
                        step.fade_samples,
                        self.cursor,
                    )
                };
                self.last_tone_gain = gain;
                // The /play sweep has unit peak digital amplitude.
                let sample = fast_sin_wrapped(self.phase) * step.digital_gain * gain;
                self.phase += self.phase_delta;
                if self.phase >= TAU {
                    self.phase -= TAU;
                }
                sample
            } else {
                0.0
            };
            match self.plan.layout.bit_depth {
                BitDepth::Pcm16 => write_pcm16_stereo(quantize_pcm16(sample), frame),
                BitDepth::Pcm24 => write_pcm24_stereo(quantize_pcm24(sample), frame),
            }
            self.cursor += 1;
        }
        frames * frame_bytes
    }

    pub fn is_finished(&self) -> bool {
        self.cursor >= self.stop.map_or(self.plan.total_samples, |stop| stop.end)
    }
    pub const fn cursor(&self) -> usize {
        self.cursor
    }
    pub const fn was_stopped(&self) -> bool {
        self.stopped
    }

    /// Idempotent. During silence finishes immediately; a tone fades from its
    /// last emitted envelope gain, keeping phase continuous and never raising gain.
    pub fn request_stop(&mut self) {
        if self.stopped || self.is_finished() {
            return;
        }
        self.stopped = true;
        self.stop = Some(match self.plan.step_index_at(self.cursor) {
            Some((index, RattlePhase::Tone { remaining_samples })) => StopFade {
                start: self.cursor,
                end: self.cursor
                    + self
                        .plan
                        .schedule
                        .tone(index)
                        .expect("validated stop tone")
                        .fade_samples
                        .min(remaining_samples),
                gain: self.last_tone_gain,
            },
            _ => StopFade {
                start: self.cursor,
                end: self.cursor,
                gain: 0.0,
            },
        });
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::playback::{PlayRequest, PlaybackPlan};
    use serde_json::json;

    fn one_tone() -> RattleRequest {
        RattleRequest {
            frequencies_hz: Some(vec![40.0]),
            tone_sec: 1.0,
            gap_sec: 0.1,
            sample_rate: 8000,
            ..RattleRequest::default()
        }
    }

    fn response(plan: &RattlePlan) -> RattleResponse {
        RattleResponse {
            started_at_us: 42,
            schedule: plan.schedule(),
            previous_volume_db: Some(-50.0),
            source_changed: true,
            previous_source: Some("Analog".into()),
        }
    }

    #[test]
    fn http_dtos_use_exact_camel_case_shapes() {
        let plan = RattlePlan::build(one_tone()).unwrap();
        let schedule = json!({
            "sampleRate": 8000,
            "leadInSamples": 2000,
            "toneSamples": 8000,
            "gapSamples": 800,
            "totalSamples": 10800,
            "frequenciesHz": [40.0],
            "volumesDb": [-35.0],
            "cycles": 1,
            "masterVolumeDb": -35.0
        });
        assert_eq!(serde_json::to_value(plan.schedule()).unwrap(), schedule);
        let response = response(&plan);
        let encoded = json!({
            "startedAtUs": 42,
            "schedule": schedule,
            "previousVolumeDb": -50.0,
            "sourceChanged": true,
            "previousSource": "Analog"
        });
        assert_eq!(serde_json::to_value(&response).unwrap(), encoded);
        assert_eq!(
            serde_json::from_value::<RattleResponse>(encoded).unwrap(),
            response
        );
        assert_eq!(
            serde_json::to_value(RattleResponse {
                previous_volume_db: None,
                source_changed: false,
                previous_source: None,
                ..response.clone()
            })
            .unwrap(),
            json!({"startedAtUs": 42, "schedule": schedule, "sourceChanged": false})
        );
        let current = plan.current_at(0).unwrap();
        let current_json = json!({
            "step": 0,
            "steps": 1,
            "label": "40 Hz @ -35 dB",
            "frequencyHz": 40.0,
            "volumeDb": -35.0,
            "cycle": 0,
            "cycles": 1,
            "phase": "leadIn",
            "remainingSec": 0.25,
            "elapsedSec": 0.0,
            "totalSec": 1.35
        });
        assert_eq!(serde_json::to_value(&current).unwrap(), current_json);
        let mut run = RattleRunStatus::started(&response);
        let run_json = json!({"startedAtUs": 42, "state": "playing", "schedule": schedule});
        assert_eq!(serde_json::to_value(&run).unwrap(), run_json);
        let status = RattleStatus {
            playing: true,
            current: Some(current),
            last: Some(run.clone()),
        };
        let status_json = json!({"playing": true, "current": current_json, "last": run_json});
        assert_eq!(serde_json::to_value(&status).unwrap(), status_json);
        assert_eq!(
            serde_json::from_value::<RattleStatus>(status_json).unwrap(),
            status
        );
        assert_eq!(
            serde_json::to_value(RattleStatus::default()).unwrap(),
            json!({"playing": false})
        );
        run.finish(Some("unplugged".into()));
        run.set_restore_errors(Some("volume failed".into()), Some("source failed".into()));
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            json!({
                "startedAtUs": 42, "state": "failed", "schedule": schedule,
                "error": "unplugged", "volumeRestoreError": "volume failed",
                "sourceRestoreError": "source failed"
            })
        );
        for (state, name) in [
            (RattleRunState::Playing, "playing"),
            (RattleRunState::Completed, "completed"),
            (RattleRunState::Stopped, "stopped"),
            (RattleRunState::Failed, "failed"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
        }
        for (phase, name) in [
            (RattleCurrentPhase::LeadIn, "leadIn"),
            (RattleCurrentPhase::Tone, "tone"),
            (RattleCurrentPhase::Gap, "gap"),
        ] {
            assert_eq!(serde_json::to_value(phase).unwrap(), json!(name));
        }
    }

    #[test]
    fn compact_schedule_derives_every_step() {
        let plan = RattlePlan::build(RattleRequest {
            frequencies_hz: Some(vec![80.0, 31.5, 40.0]),
            volumes_db: vec![-20.0, -40.0, -30.0],
            cycles: 3,
            ..one_tone()
        })
        .unwrap();
        let schedule = plan.schedule();
        assert_eq!(schedule.frequencies_hz, vec![80.0, 31.5, 40.0]);
        assert_eq!(schedule.volumes_db, vec![-40.0, -30.0, -20.0]);
        assert_eq!(schedule.master_volume_db, -20.0);
        assert_eq!(schedule.steps(), 27);
        for step in plan.steps() {
            assert_eq!(schedule.step(step.index).as_ref(), Some(&step));
            let current = schedule.current_at(step.start_sample).unwrap();
            assert_eq!(current, plan.current_at(step.start_sample).unwrap());
            assert_eq!(current.step, step.index);
            assert_eq!(current.cycle, step.cycle);
            assert_eq!(current.frequency_hz, step.frequency_hz);
            assert_eq!(current.volume_db, step.volume_db);
            assert_eq!(current.label, step.label);
            assert_eq!(current.phase, RattleCurrentPhase::Tone);
            let gap = schedule.current_at(step.end_sample).unwrap();
            assert_eq!(gap.step, step.index);
            assert_eq!(gap.phase, RattleCurrentPhase::Gap);
        }
        assert!(schedule.step(schedule.steps()).is_none());
        assert!(schedule.step(usize::MAX).is_none());
        let zero = RattlePlan::build(RattleRequest {
            volumes_db: vec![-0.0],
            ..one_tone()
        })
        .unwrap();
        assert_eq!(zero.schedule().step(0).unwrap(), zero.step(0).unwrap());
    }

    #[test]
    fn current_phase_boundaries_and_end() {
        let plan = RattlePlan::build(one_tone()).unwrap();
        let schedule = plan.schedule();
        for (sample, phase, remaining) in [
            (0, RattleCurrentPhase::LeadIn, 2000),
            (1999, RattleCurrentPhase::LeadIn, 1),
            (2000, RattleCurrentPhase::Tone, 8000),
            (9999, RattleCurrentPhase::Tone, 1),
            (10000, RattleCurrentPhase::Gap, 800),
            (10799, RattleCurrentPhase::Gap, 1),
        ] {
            let current = schedule.current_at(sample).unwrap();
            assert_eq!(current.phase, phase);
            assert_eq!(current.step, 0);
            assert_eq!(current.remaining_sec, remaining as f64 / 8000.0);
            assert_eq!(current.elapsed_sec, sample as f64 / 8000.0);
            assert_eq!(current.total_sec, 1.35);
        }
        assert!(schedule.current_at(10800).is_none());
        assert!(schedule.current_at(usize::MAX).is_none());
        let no_gap = RattlePlan::build(RattleRequest {
            cycles: 2,
            gap_sec: 0.0,
            ..one_tone()
        })
        .unwrap();
        let next = no_gap
            .current_at(no_gap.step(0).unwrap().end_sample)
            .unwrap();
        assert_eq!(next.step, 1);
        assert_eq!(next.cycle, 1);
        assert_eq!(next.phase, RattleCurrentPhase::Tone);
        assert!(no_gap.current_at(no_gap.total_samples()).is_none());
    }

    #[test]
    fn elapsed_samples_clamps_and_avoids_overflow() {
        assert_eq!(elapsed_samples(100, 99, 48000, 0), 0);
        assert_eq!(elapsed_samples(100, 100, 48000, 0), 0);
        assert_eq!(elapsed_samples(100, 200, 48000, 101), 0);
        assert_eq!(elapsed_samples(100, 200, 48000, 100), 0);
        assert_eq!(elapsed_samples(100, 300, 48000, 100), 4);
        assert_eq!(elapsed_samples(0, 20, 48000, 0), 0);
        assert_eq!(elapsed_samples(0, 21, 48000, 0), 1);
        assert_eq!(elapsed_samples(0, 0, 48000, -100), 4);
        assert_eq!(elapsed_samples(-100, 0, 48000, 0), 4);
        assert_eq!(elapsed_samples(0, 3_600_000_000, 384000, 0), 1_382_400_000);
        assert_eq!(
            elapsed_samples(i64::MIN, i64::MAX, u32::MAX, i64::MIN),
            usize::MAX
        );
        assert_eq!(elapsed_samples(i64::MAX, i64::MIN, u32::MAX, i64::MAX), 0);
        assert_eq!(elapsed_samples(i64::MIN, i64::MAX, 0, 0), 0);
    }

    #[test]
    fn run_state_settles_and_reports_restore_errors_separately() {
        let response = response(&RattlePlan::build(one_tone()).unwrap());
        for state in [
            RattleRunState::Completed,
            RattleRunState::Stopped,
            RattleRunState::Failed,
        ] {
            let mut run = RattleRunStatus::started(&response);
            assert_eq!(run.started_at_us, response.started_at_us);
            assert_eq!(run.schedule, response.schedule);
            assert_eq!(run.state, RattleRunState::Playing);
            assert!(run.error.is_none());
            assert!(run.volume_restore_error.is_none());
            assert!(run.source_restore_error.is_none());
            match state {
                RattleRunState::Stopped => run.stopped(),
                RattleRunState::Failed => run.finish(Some("stream failed".into())),
                _ => run.finish(None),
            }
            assert_eq!(run.state, state);
            assert_eq!(run.error.is_some(), state == RattleRunState::Failed);
            run.set_restore_errors(Some("volume failed".into()), Some("source failed".into()));
            assert_eq!(run.state, state);
            assert_eq!(run.volume_restore_error.as_deref(), Some("volume failed"));
            assert_eq!(run.source_restore_error.as_deref(), Some("source failed"));
            run.set_restore_errors(None, None);
            assert!(run.volume_restore_error.is_none());
            assert!(run.source_restore_error.is_none());
        }
    }

    #[test]
    fn maximum_step_responses_stay_compact() {
        let plan = RattlePlan::build(RattleRequest {
            frequencies_hz: Some((20..84).map(|hz| hz as f32).collect()),
            volumes_db: vec![-20.0, -25.0, -30.0, -35.0, -40.0, -45.0, -50.0, -55.0],
            cycles: 8,
            tone_sec: 0.5,
            gap_sec: 0.0,
            ..one_tone()
        })
        .unwrap();
        assert_eq!(plan.schedule().steps(), 4096);
        let response = response(&plan);
        assert!(serde_json::to_vec(&response).unwrap().len() < 3072);
        let mut status = RattleStatus {
            playing: true,
            current: plan.current_at(0),
            last: Some(RattleRunStatus::started(&response)),
        };
        assert!(serde_json::to_vec(&status).unwrap().len() < 3072);
        status.playing = false;
        status.current = None;
        status.last.as_mut().unwrap().finish(None);
        assert!(serde_json::to_vec(&status).unwrap().len() < 3072);
    }

    fn render(plan: &RattlePlan) -> Vec<u8> {
        let mut generator = plan.generator();
        let mut pcm = Vec::new();
        // Odd buffer size exercises partial frames and boundaries across calls.
        let mut buffer = [0xa5; 997];
        while !generator.is_finished() {
            let written = generator.fill_pcm_bytes(&mut buffer);
            assert!(written > 0);
            assert_eq!(written % plan.layout().frame_bytes(), 0);
            assert!(buffer[written..].iter().all(|b| *b == 0xa5));
            pcm.extend_from_slice(&buffer[..written]);
            buffer.fill(0xa5);
        }
        assert_eq!(generator.cursor(), plan.total_samples());
        assert_eq!(generator.fill_pcm_bytes(&mut buffer), 0);
        assert!(!generator.was_stopped());
        pcm
    }

    fn pcm16(pcm: &[u8]) -> Vec<i16> {
        pcm.as_chunks::<4>()
            .0
            .iter()
            .map(|frame| {
                assert_eq!(&frame[..2], &frame[2..]);
                i16::from_le_bytes([frame[0], frame[1]])
            })
            .collect()
    }

    fn advance(generator: &mut RattleGenerator, samples: usize) {
        let mut buffer = vec![0; samples * generator.plan.layout().frame_bytes()];
        assert_eq!(generator.fill_pcm_bytes(&mut buffer), buffer.len());
    }

    #[test]
    fn defaults_schedule_labels_and_json() {
        let request: RattleRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(request, RattleRequest::default());
        let plan = RattlePlan::build(request).unwrap();
        assert_eq!(plan.steps().len(), 11);
        assert_eq!(plan.sample_rate(), 48000);
        assert_eq!(plan.layout().bit_depth, BitDepth::Pcm16);
        assert_eq!(plan.total_samples(), 2_652_000);
        assert_eq!(plan.master_volume_db(), -35.0);
        for (i, step) in plan.steps().enumerate() {
            assert_eq!(step.index, i);
            assert_eq!(step.cycle, 0);
            assert_eq!(step.frequency_hz, DEFAULT_FREQUENCIES_HZ[i]);
            assert_eq!(step.start_sample, 12000 + i * 240000);
            assert_eq!(step.end_sample, step.start_sample + 192000);
            assert_eq!(step.gap_end_sample, step.end_sample + 48000);
            assert_eq!(step.digital_gain, 1.0);
        }
        assert_eq!(plan.step(2).unwrap().label, "31.5 Hz @ -35 dB");
        assert_eq!(plan.step(3).unwrap().label, "40 Hz @ -35 dB");
        let encoded = serde_json::to_value(plan.step(0).unwrap()).unwrap();
        assert_eq!(encoded["frequencyHz"], 20.0);
        assert_eq!(encoded["gapEndSample"], 252000);
        assert_eq!(encoded["digitalGain"], 1.0);
        let roundtrip = serde_json::to_value(RattleRequest::default()).unwrap();
        assert_eq!(
            serde_json::from_value::<RattleRequest>(roundtrip).unwrap(),
            RattleRequest::default()
        );
        assert!(serde_json::from_value::<RattleRequest>(json!({"frequencies": [40]})).is_err());
        assert!(serde_json::from_value::<RattleRequest>(json!({"bitDepth": "32"})).is_err());
    }

    #[test]
    fn logarithmic_form() {
        let request: RattleRequest =
            serde_json::from_value(json!({"startHz":20, "endHz":160, "count":4})).unwrap();
        let plan = RattlePlan::build(request.clone()).unwrap();
        for (step, expected) in plan.steps().zip([20.0, 40.0, 80.0, 160.0]) {
            assert!((step.frequency_hz - expected).abs() < 0.0001);
        }
        let single = RattlePlan::build(RattleRequest {
            count: Some(1),
            ..request
        })
        .unwrap();
        assert_eq!(single.step(0).unwrap().frequency_hz, 20.0);
        assert_eq!(single.steps().len(), 1);
    }

    #[test]
    fn fractional_volumes_must_match_hardware_steps() {
        for volume in [-35.2, -35.01, -0.1, -126.99] {
            let error = RattlePlan::build(RattleRequest {
                volumes_db: vec![volume],
                ..one_tone()
            })
            .unwrap_err();
            assert!(error.to_string().contains("multiples of 0.5 dB"));
        }
        for volume in [-127.0, -35.5, -35.0, -0.5, 0.0] {
            let plan = RattlePlan::build(RattleRequest {
                volumes_db: vec![volume],
                ..one_tone()
            })
            .unwrap();
            assert_eq!(plan.master_volume_db(), volume);
            assert_eq!(plan.step(0).unwrap().digital_gain, 1.0);
        }
    }

    #[test]
    fn generated_frequencies_are_rounded_and_labels_match_played_tones() {
        let request = RattleRequest {
            frequencies_hz: None,
            start_hz: Some(20.0),
            end_hz: Some(200.0),
            count: Some(64),
            ..one_tone()
        };
        let plan = RattlePlan::build(request.clone()).unwrap();
        assert_eq!(plan.step(1).unwrap().frequency_hz, 20.7);
        assert_eq!(plan.step(1).unwrap().label, "20.7 Hz @ -35 dB");
        assert_eq!(plan.step(0).unwrap().label, "20 Hz @ -35 dB");
        for step in plan.steps() {
            assert_eq!(step.frequency_hz, (step.frequency_hz * 10.0).round() / 10.0);
            assert_eq!(
                plan.schedule.tone(step.index).unwrap().frequency_hz,
                step.frequency_hz
            );
        }
        for (start, end) in [(20.0, 20.0), (20.0, 20.04)] {
            let error = RattlePlan::build(RattleRequest {
                start_hz: Some(start),
                end_hz: Some(end),
                count: Some(2),
                ..request.clone()
            })
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("unique tones after rounding to 0.1 Hz")
            );
        }
    }

    #[test]
    fn generated_tone_count_is_bounded() {
        for (count, tone_sec) in [(65, 0.5), (600_000_000, 2.0 / 384000.0)] {
            assert!(matches!(
                RattlePlan::build(RattleRequest {
                    frequencies_hz: None,
                    start_hz: Some(20.0),
                    end_hz: Some(200.0),
                    count: Some(count),
                    tone_sec,
                    gap_sec: 0.0,
                    sample_rate: 384000,
                    ..one_tone()
                }),
                Err(PlaybackError::InvalidRequest(_))
            ));
        }
        assert!(
            RattlePlan::build(RattleRequest {
                frequencies_hz: None,
                start_hz: Some(20.0),
                end_hz: Some(200.0),
                count: Some(64),
                tone_sec: 0.5,
                ..one_tone()
            })
            .is_ok()
        );
    }

    #[test]
    fn explicit_tone_count_is_bounded() {
        assert!(matches!(
            RattlePlan::build(RattleRequest {
                frequencies_hz: Some(vec![40.0; 65]),
                tone_sec: 0.5,
                ..one_tone()
            }),
            Err(PlaybackError::InvalidRequest(_))
        ));
        assert!(
            RattlePlan::build(RattleRequest {
                frequencies_hz: Some(vec![40.0; 64]),
                tone_sec: 0.5,
                ..one_tone()
            })
            .is_ok()
        );
    }

    #[test]
    fn total_steps_are_bounded_before_allocation() {
        let request = RattleRequest {
            tone_sec: 0.5,
            gap_sec: 0.0,
            ..one_tone()
        };
        for (tones, volumes, cycles) in [(1, 1, 4097), (1, 4097, 1), (64, 65, 1)] {
            assert!(matches!(
                RattlePlan::build(RattleRequest {
                    frequencies_hz: Some(vec![40.0; tones]),
                    volumes_db: vec![-35.0; volumes],
                    cycles,
                    ..request.clone()
                }),
                Err(PlaybackError::InvalidRequest(_))
            ));
        }
        let plan = RattlePlan::build(RattleRequest {
            frequencies_hz: Some(vec![40.0; 64]),
            volumes_db: vec![-35.0; 32],
            cycles: 2,
            ..request
        })
        .unwrap();
        assert_eq!(plan.steps().len(), 4096);
    }

    #[test]
    fn minimum_tone_duration() {
        for tone_sec in [2.0 / 384000.0, 0.499999] {
            assert!(matches!(
                RattlePlan::build(RattleRequest {
                    tone_sec,
                    gap_sec: 0.0,
                    sample_rate: 384000,
                    ..one_tone()
                }),
                Err(PlaybackError::InvalidRequest(_))
            ));
        }
        assert!(
            RattlePlan::build(RattleRequest {
                tone_sec: 0.5,
                ..one_tone()
            })
            .is_ok()
        );
    }

    #[test]
    fn labels_normalize_negative_zero() {
        let plan = RattlePlan::build(RattleRequest {
            volumes_db: vec![-0.0],
            ..one_tone()
        })
        .unwrap();
        assert_eq!(plan.step(0).unwrap().label, "40 Hz @ 0 dB");
    }

    #[test]
    fn long_tones_keep_phase_accurate() {
        for frequency in [5.0, 2000.0] {
            let plan = RattlePlan::build(RattleRequest {
                frequencies_hz: Some(vec![frequency]),
                tone_sec: 60.0,
                gap_sec: 0.0,
                sample_rate: 384000,
                ..one_tone()
            })
            .unwrap();
            let step = &plan.step(0).unwrap();
            let mut generator = plan.generator();
            advance(&mut generator, step.start_sample);
            // Cross phase anchors and buffer boundaries at different positions.
            let mut bytes = [0; 997 * 4];
            while !generator.is_finished() {
                assert!(generator.fill_pcm_bytes(&mut bytes) > 0);
                let local = generator.cursor() - step.start_sample;
                let expected = (core::f64::consts::TAU * frequency as f64 * local as f64
                    / plan.sample_rate() as f64)
                    % core::f64::consts::TAU;
                let difference = (generator.phase as f64 - expected).abs();
                let error = difference.min(core::f64::consts::TAU - difference);
                assert!(
                    error < 0.0003,
                    "{frequency} Hz after {local} samples: phase error {error} rad"
                );
            }
            assert_eq!(generator.cursor(), step.end_sample);
        }
    }

    #[test]
    fn stop_envelope_never_increases_at_any_fade_position() {
        let plan = RattlePlan::build(RattleRequest {
            frequencies_hz: Some(vec![2000.0]),
            ..one_tone()
        })
        .unwrap();
        let step = &plan.step(0).unwrap();
        let tone_samples = step.end_sample - step.start_sample;
        let mut source = plan.generator();
        advance(&mut source, step.start_sample);
        for local in 0..=tone_samples {
            if local <= step.fade_samples || local >= tone_samples - step.fade_samples {
                let mut generator = source.clone();
                let mut previous_gain = if local == 0 {
                    0.0
                } else {
                    tone_gain(
                        step.start_sample,
                        step.end_sample,
                        step.fade_samples,
                        step.start_sample + local - 1,
                    )
                };
                generator.request_stop();
                let stop = generator.stop.unwrap();
                let mut previous_peak = previous_gain;
                for index in 0..stop.end - stop.start {
                    let gain =
                        stop.gain * raised_cosine_fade_out_gain(index, stop.end - stop.start);
                    assert!(
                        gain <= previous_gain,
                        "stop at {local}, fade sample {index}: {gain} > {previous_gain}"
                    );
                    let mut bytes = [0; 4];
                    assert_eq!(generator.fill_pcm_bytes(&mut bytes), bytes.len());
                    let amplitude = i16::from_le_bytes([bytes[0], bytes[1]]).unsigned_abs() as f32
                        / i16::MAX as f32;
                    assert!(amplitude <= gain + 1.0 / i16::MAX as f32);
                    // At Fs/4 every odd sample is a carrier peak, exposing the
                    // actual PCM envelope independently of its zero crossings.
                    if (local + index) % 2 == 1 {
                        assert!(amplitude <= previous_peak + 1.0 / i16::MAX as f32);
                        previous_peak = amplitude;
                    }
                    previous_gain = gain;
                }
                assert!(generator.is_finished());
            }
            if local < tone_samples {
                advance(&mut source, 1);
            }
        }
    }

    #[test]
    fn invalid_requests() {
        let invalid = [
            json!({"frequenciesHz":[40], "startHz":20,"endHz":80,"count":3}),
            json!({"startHz":20}),
            json!({"startHz":80,"endHz":20,"count":3}),
            json!({"startHz":20,"endHz":80,"count":0}),
            json!({"frequenciesHz":[]}),
            json!({"frequenciesHz":[0]}),
            json!({"frequenciesHz":[4.9]}),
            json!({"frequenciesHz":[2000.1]}),
            json!({"frequenciesHz":[40], "sampleRate":80}),
            json!({"toneSec":0}),
            json!({"toneSec":-1}),
            json!({"toneSec":601}),
            json!({"toneSec":0.000001}),
            json!({"gapSec":-1}),
            json!({"gapSec":601}),
            json!({"cycles":0}),
            json!({"volumesDb":[]}),
            json!({"volumesDb":[-128]}),
            json!({"volumesDb":[0.1]}),
            json!({"sampleRate":0}),
            json!({"sampleRate":384001}),
            json!({"cycles":66}),
            json!({"startHz":20,"endHz":80,"count":4294967295u32}),
        ];
        for value in invalid {
            let request = serde_json::from_value(value.clone()).unwrap();
            assert!(RattlePlan::build(request).is_err(), "accepted {value}");
        }
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for request in [
                RattleRequest {
                    frequencies_hz: Some(vec![bad]),
                    ..one_tone()
                },
                RattleRequest {
                    volumes_db: vec![bad],
                    ..one_tone()
                },
                RattleRequest {
                    tone_sec: bad,
                    ..one_tone()
                },
                RattleRequest {
                    gap_sec: bad,
                    ..one_tone()
                },
                RattleRequest {
                    frequencies_hz: None,
                    start_hz: Some(bad),
                    end_hz: Some(100.0),
                    count: Some(2),
                    ..one_tone()
                },
            ] {
                assert!(RattlePlan::build(request).is_err());
            }
        }
        // Valid offsets at the maximum supported rate remain within u32.
        let plan = RattlePlan::build(RattleRequest {
            cycles: 65,
            sample_rate: 384000,
            ..RattleRequest::default()
        })
        .unwrap();
        assert!(plan.total_samples() <= u32::MAX as usize);
        assert!(plan.total_samples() <= 3600 * 384000);
        assert!(
            RattlePlan::build(RattleRequest {
                volumes_db: vec![-127.0, 0.0],
                ..one_tone()
            })
            .is_ok()
        );
        // Check the rounded sample cap as well as nominal seconds.
        assert!(
            RattlePlan::build(RattleRequest {
                frequencies_hz: Some(vec![5.0]),
                tone_sec: 0.5,
                gap_sec: 0.32,
                cycles: 4000,
                sample_rate: 11,
                ..one_tone()
            })
            .is_err()
        );
    }

    #[test]
    fn cycle_volume_tone_order_and_gains() {
        let plan = RattlePlan::build(RattleRequest {
            frequencies_hz: Some(vec![40.0, 31.5]),
            volumes_db: vec![-20.0, -40.0, -30.0],
            cycles: 2,
            ..one_tone()
        })
        .unwrap();
        assert_eq!(plan.master_volume_db(), -20.0);
        assert_eq!(plan.steps().len(), 12);
        for (i, step) in plan.steps().enumerate() {
            assert_eq!(step.cycle, (i / 6) as u32);
            assert_eq!(step.volume_db, [-40.0, -30.0, -20.0][i % 6 / 2]);
            assert_eq!(step.frequency_hz, [40.0, 31.5][i % 2]);
            let expected = [0.1, 0.31622776, 1.0][i % 6 / 2];
            assert!((step.digital_gain - expected).abs() < 1e-6);
        }
        assert_eq!(plan.step(1).unwrap().label, "31.5 Hz @ -40 dB");
    }

    #[test]
    fn phase_boundaries() {
        let plan = RattlePlan::build(RattleRequest {
            cycles: 2,
            ..one_tone()
        })
        .unwrap();
        let step = &plan.step(0).unwrap();
        assert_eq!(plan.step_at(0), None);
        assert_eq!(plan.step_at(step.start_sample - 1), None);
        assert_eq!(
            plan.step_at(step.start_sample),
            Some((
                (*step).clone(),
                RattlePhase::Tone {
                    remaining_samples: 8000
                }
            ))
        );
        assert_eq!(
            plan.step_at(step.end_sample - 1),
            Some((
                (*step).clone(),
                RattlePhase::Tone {
                    remaining_samples: 1
                }
            ))
        );
        assert_eq!(
            plan.step_at(step.end_sample),
            Some((
                (*step).clone(),
                RattlePhase::Gap {
                    remaining_samples: 800
                }
            ))
        );
        assert_eq!(
            plan.step_at(step.gap_end_sample - 1),
            Some((
                (*step).clone(),
                RattlePhase::Gap {
                    remaining_samples: 1
                }
            ))
        );
        assert_eq!(plan.step_at(step.gap_end_sample).unwrap().0.index, 1);
        assert_eq!(plan.step_at(plan.total_samples()), None);
        assert_eq!(plan.step_at(usize::MAX), None);
        let no_gap = RattlePlan::build(RattleRequest {
            gap_sec: 0.0,
            cycles: 2,
            ..one_tone()
        })
        .unwrap();
        assert!(matches!(
            no_gap.step_at(no_gap.step(0).unwrap().end_sample),
            Some((RattleStep { index: 1, .. }, RattlePhase::Tone { .. }))
        ));
    }

    #[test]
    fn generated_signal_level_frequency_and_reference() {
        let plan = RattlePlan::build(RattleRequest {
            volumes_db: vec![-40.0, -20.0],
            cycles: 2,
            ..one_tone()
        })
        .unwrap();
        let samples = pcm16(&render(&plan));
        assert_eq!(samples.len(), plan.total_samples());
        assert!(
            samples[..plan.step(0).unwrap().start_sample]
                .iter()
                .all(|s| *s == 0)
        );
        for step in plan.steps() {
            assert_eq!(samples[step.start_sample], 0);
            assert_eq!(samples[step.end_sample - 1], 0);
            assert!(
                samples[step.end_sample..step.gap_end_sample]
                    .iter()
                    .all(|s| *s == 0)
            );
            let tone = &samples[step.start_sample..step.end_sample];
            let peak =
                tone.iter().map(|s| (*s as i32).abs()).max().unwrap() as f32 / i16::MAX as f32;
            assert!((peak - step.digital_gain).abs() < 0.0002);
            let crossings = tone
                .windows(2)
                .filter(|pair| pair[0] <= 0 && pair[1] > 0)
                .count();
            assert_eq!(crossings, 40);
            for (i, &sample) in tone.iter().enumerate() {
                let phase = TAU * step.frequency_hz * i as f32 / plan.sample_rate() as f32;
                // Independent envelope formula, with both edge samples at zero.
                let fade_position = i.min(tone.len() - 1 - i);
                let envelope = if fade_position < step.fade_samples {
                    0.5 * (1.0
                        - (core::f32::consts::PI * fade_position as f32
                            / (step.fade_samples - 1) as f32)
                            .cos())
                } else {
                    1.0
                };
                let reference = phase.sin() * step.digital_gain * envelope;
                assert!((sample as f32 / i16::MAX as f32 - reference).abs() < 0.001);
            }
        }
        let sweep = PlaybackPlan::build(PlayRequest {
            marker_before: false,
            marker_after: false,
            ..PlayRequest::default()
        })
        .unwrap();
        let sweep_peak = (0..sweep.sweep_samples)
            .map(|i| {
                sweep
                    .sample_at(sweep.offsets().sweep_offset_samples + i)
                    .abs()
            })
            .fold(0.0f32, f32::max);
        assert!((sweep_peak - 1.0).abs() < 0.0001);
    }

    #[test]
    fn fade_duration_rule_and_short_tones() {
        for (frequency, tone_sec, expected) in [
            (5.0, 1.0, 3200),
            (20.0, 1.0, 800),
            (40.0, 1.0, 800),
            (5.0, 0.5, 2000),
        ] {
            let plan = RattlePlan::build(RattleRequest {
                frequencies_hz: Some(vec![frequency]),
                tone_sec,
                ..one_tone()
            })
            .unwrap();
            let step = &plan.step(0).unwrap();
            assert_eq!(step.fade_samples, expected);
            assert_eq!(
                tone_gain(
                    step.start_sample,
                    step.end_sample,
                    step.fade_samples,
                    step.start_sample
                ),
                0.0
            );
            assert_eq!(
                tone_gain(
                    step.start_sample,
                    step.end_sample,
                    step.fade_samples,
                    step.end_sample - 1
                ),
                0.0
            );
            assert_eq!(
                tone_gain(
                    step.start_sample,
                    step.end_sample,
                    step.fade_samples,
                    step.start_sample + expected - 1
                ),
                1.0
            );
            assert_eq!(
                tone_gain(
                    step.start_sample,
                    step.end_sample,
                    step.fade_samples,
                    step.end_sample - expected
                ),
                1.0
            );
        }
    }

    #[test]
    fn stop_in_hold_fade_in_and_fade_out() {
        let plan = RattlePlan::build(one_tone()).unwrap();
        let step = &plan.step(0).unwrap();
        for local in [step.fade_samples / 4, 2500, 8000 - step.fade_samples / 2] {
            let mut generator = plan.generator();
            let start = step.start_sample + local;
            advance(&mut generator, start);
            let phase = generator.phase;
            let gain = tone_gain(
                step.start_sample,
                step.end_sample,
                step.fade_samples,
                start - 1,
            );
            generator.request_stop();
            generator.request_stop();
            assert!(generator.was_stopped());
            let stop = generator.stop.unwrap();
            assert_eq!(stop.gain, gain);
            assert_eq!(
                stop.end - start,
                step.fade_samples.min(step.end_sample - start)
            );
            let mut bytes = vec![0; (stop.end - start + 10) * 4];
            assert_eq!(generator.fill_pcm_bytes(&mut bytes), (stop.end - start) * 4);
            let samples = pcm16(&bytes[..(stop.end - start) * 4]);
            assert_eq!(samples[0], quantize_pcm16(fast_sin_wrapped(phase) * gain));
            assert_eq!(*samples.last().unwrap(), 0);
            assert!(
                samples
                    .iter()
                    .all(|s| (*s as f32).abs() <= gain * i16::MAX as f32 + 1.0)
            );
            assert!(generator.is_finished());
            assert_eq!(generator.cursor(), stop.end);
            assert_eq!(generator.fill_pcm_bytes(&mut bytes), 0);
        }
    }

    #[test]
    fn stop_during_silence_and_after_completion() {
        let plan = RattlePlan::build(one_tone()).unwrap();
        for cursor in [
            0,
            plan.step(0).unwrap().end_sample,
            plan.step(0).unwrap().gap_end_sample - 1,
        ] {
            let mut generator = plan.generator();
            advance(&mut generator, cursor);
            generator.request_stop();
            assert!(generator.is_finished());
            assert!(generator.was_stopped());
            assert_eq!(generator.cursor(), cursor);
            assert_eq!(generator.fill_pcm_bytes(&mut [1; 40]), 0);
        }
        let mut generator = plan.generator();
        advance(&mut generator, plan.total_samples());
        generator.request_stop();
        assert!(!generator.was_stopped());
    }

    #[test]
    fn pcm24_stereo_and_chunk_independence() {
        let plan = RattlePlan::build(RattleRequest {
            bit_depth: BitDepth::Pcm24,
            ..one_tone()
        })
        .unwrap();
        let bytes = render(&plan);
        assert_eq!(bytes.len(), plan.total_samples() * 6);
        let mut all_at_once = vec![0; bytes.len()];
        assert_eq!(
            plan.generator().fill_pcm_bytes(&mut all_at_once),
            bytes.len()
        );
        assert_eq!(bytes, all_at_once);
        let samples: Vec<i32> = bytes
            .as_chunks::<6>()
            .0
            .iter()
            .map(|f| {
                assert_eq!(&f[..3], &f[3..]);
                (i32::from_le_bytes([f[0], f[1], f[2], 0]) << 8) >> 8
            })
            .collect();
        let step = &plan.step(0).unwrap();
        assert!(samples[..step.start_sample].iter().all(|s| *s == 0));
        assert!(samples[step.end_sample..].iter().all(|s| *s == 0));
        assert_eq!(samples[step.start_sample], 0);
        assert_eq!(samples[step.end_sample - 1], 0);
        assert!(
            (samples.iter().map(|s| s.abs()).max().unwrap() as f32 / 8_388_607.0 - 1.0).abs()
                < 0.0001
        );
        assert_eq!(plan.generator().fill_pcm_bytes(&mut [1; 5]), 0);
    }
}
