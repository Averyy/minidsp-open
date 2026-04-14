//! Desktop audio playback backend for `/play`.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use minidsp_protocol::playback::{BitDepth, PlayResponse, PlaybackGenerator, PlaybackPlan};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

const START_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("miniDSP desktop audio output device not found")]
    NoOutputDevice,
    #[error("failed to enumerate audio devices: {0}")]
    EnumerateDevices(String),
    #[error("failed to read output device name: {0}")]
    DeviceName(String),
    #[error("failed to build output stream config: {0}")]
    Config(String),
    #[error("unsupported output sample format")]
    UnsupportedSampleFormat,
    #[error("failed to build output stream: {0}")]
    BuildStream(String),
    #[error("failed to start output stream: {0}")]
    StartStream(String),
    #[error("playback did not start before timeout")]
    StartTimeout,
    #[error("desktop playback stream failed: {0}")]
    StreamError(String),
}

pub fn run_playback(
    plan: PlaybackPlan,
    source_changed: bool,
    previous_source: Option<String>,
    started_tx: oneshot::Sender<Result<PlayResponse, String>>,
) -> Result<(), PlaybackError> {
    let device = match select_output_device() {
        Ok(device) => device,
        Err(err) => {
            let _ = started_tx.send(Err(err.to_string()));
            return Err(err);
        }
    };
    let stream_config = match select_stream_config(&device, &plan) {
        Ok(config) => config,
        Err(err) => {
            let _ = started_tx.send(Err(err.to_string()));
            return Err(err);
        }
    };
    let layout = plan.layout();
    let response = PlayResponse {
        started_at_us: 0,
        tone_burst_offset_samples: plan.offsets().tone_burst_offset_samples,
        sweep_offset_samples: plan.offsets().sweep_offset_samples,
        tail_tone_burst_offset_samples: plan.offsets().tail_tone_burst_offset_samples,
        total_samples: plan.offsets().total_samples,
        sample_rate: plan.request.sample_rate,
        source_changed,
        previous_source,
    };

    let generator = Arc::new(Mutex::new(plan.generator()));
    let lifecycle = Arc::new(PlaybackLifecycle::default());
    let finished = Arc::new(AtomicBool::new(false));
    let stream = match build_stream(
        &device,
        &stream_config,
        layout.bit_depth,
        generator,
        lifecycle.clone(),
        finished.clone(),
    ) {
        Ok(stream) => stream,
        Err(err) => {
            let _ = started_tx.send(Err(err.to_string()));
            return Err(err);
        }
    };
    if let Err(err) = stream
        .play()
        .map_err(|e| PlaybackError::StartStream(e.to_string()))
    {
        let _ = started_tx.send(Err(err.to_string()));
        return Err(err);
    }

    let started_at_us = match lifecycle.wait_for_start() {
        Ok(started_at_us) => started_at_us,
        Err(err) => {
            let _ = started_tx.send(Err(err.to_string()));
            return Err(err);
        }
    };
    let _ = started_tx.send(Ok(PlayResponse {
        started_at_us,
        ..response
    }));
    while !finished.load(Ordering::SeqCst) {
        if let Some(err) = lifecycle.take_error() {
            return Err(PlaybackError::StreamError(err));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(err) = lifecycle.take_error() {
        return Err(PlaybackError::StreamError(err));
    }
    // Give the host a brief moment to drain the last callback buffer.
    std::thread::sleep(Duration::from_millis(50));
    drop(stream);

    Ok(())
}

#[derive(Default)]
struct PlaybackLifecycle {
    started_at_us: Mutex<Option<i64>>,
    error: Mutex<Option<String>>,
    cv: Condvar,
}

impl PlaybackLifecycle {
    fn mark_started(&self) {
        let mut guard = self.started_at_us.lock().expect("started mutex poisoned");
        if guard.is_none() {
            *guard = Some(server_time_us());
            self.cv.notify_all();
        }
    }

    fn mark_error(&self, err: String) {
        let mut guard = self.error.lock().expect("error mutex poisoned");
        if guard.is_none() {
            *guard = Some(err);
        }
        self.cv.notify_all();
    }

    fn take_error(&self) -> Option<String> {
        self.error.lock().expect("error mutex poisoned").take()
    }

    fn wait_for_start(&self) -> Result<i64, PlaybackError> {
        let deadline = std::time::Instant::now() + START_TIMEOUT;
        loop {
            if let Some(err) = self.take_error() {
                return Err(PlaybackError::StreamError(err));
            }

            let mut guard = self.started_at_us.lock().expect("started mutex poisoned");
            if let Some(started_at_us) = *guard {
                return Ok(started_at_us);
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(PlaybackError::StartTimeout);
            }
            let timeout = deadline.saturating_duration_since(now);
            let (next, _) = self
                .cv
                .wait_timeout(guard, timeout)
                .expect("started condvar poisoned");
            guard = next;
            if let Some(started_at_us) = *guard {
                return Ok(started_at_us);
            }
        }
    }
}

pub(crate) fn server_time_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_default()
}

fn select_output_device() -> Result<cpal::Device, PlaybackError> {
    let host = cpal::default_host();
    let device_hint = std::env::var("MINIDSP_PLAY_DEVICE").ok();
    if let Some(hint) = device_hint.as_deref() {
        let hint_lower = hint.to_lowercase();
        let devices = host
            .output_devices()
            .map_err(|e| PlaybackError::EnumerateDevices(e.to_string()))?;
        for device in devices {
            let name = device
                .name()
                .map_err(|e| PlaybackError::DeviceName(e.to_string()))?;
            if name.to_lowercase().contains(&hint_lower) {
                return Ok(device);
            }
        }
        return Err(PlaybackError::NoOutputDevice);
    }

    let devices = host
        .output_devices()
        .map_err(|e| PlaybackError::EnumerateDevices(e.to_string()))?;
    for device in devices {
        let name = device
            .name()
            .map_err(|e| PlaybackError::DeviceName(e.to_string()))?;
        if name.to_lowercase().contains("minidsp") {
            return Ok(device);
        }
    }

    Err(PlaybackError::NoOutputDevice)
}

fn select_stream_config(
    device: &cpal::Device,
    plan: &PlaybackPlan,
) -> Result<cpal::SupportedStreamConfig, PlaybackError> {
    let mut fallback = None;
    let configs = device
        .supported_output_configs()
        .map_err(|e| PlaybackError::Config(e.to_string()))?;
    for cfg in configs {
        if cfg.channels() != 2 {
            continue;
        }
        let min = cfg.min_sample_rate().0;
        let max = cfg.max_sample_rate().0;
        if plan.request.sample_rate < min || plan.request.sample_rate > max {
            continue;
        }
        let exact = cfg.with_sample_rate(cpal::SampleRate(plan.request.sample_rate));
        if matches!(exact.sample_format(), cpal::SampleFormat::F32) {
            return Ok(exact);
        }
        if fallback.is_none() {
            fallback = Some(exact);
        }
    }
    fallback.ok_or_else(|| {
        PlaybackError::Config(format!(
            "no stereo output config supports {} Hz",
            plan.request.sample_rate
        ))
    })
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    bit_depth: BitDepth,
    generator: Arc<Mutex<PlaybackGenerator>>,
    lifecycle: Arc<PlaybackLifecycle>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, PlaybackError> {
    let err_lifecycle = lifecycle.clone();
    let err_finished = finished.clone();
    let err_fn = move |err: cpal::StreamError| {
        tracing::warn!("desktop playback stream error: {err}");
        err_lifecycle.mark_error(err.to_string());
        err_finished.store(true, Ordering::SeqCst);
    };
    match config.sample_format() {
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                &config.config(),
                move |data: &mut [f32], _| {
                    write_output_data(data, bit_depth, &generator, &lifecycle, &finished)
                },
                err_fn,
                None,
            )
            .map_err(|e| PlaybackError::BuildStream(e.to_string())),
        cpal::SampleFormat::I16 => device
            .build_output_stream(
                &config.config(),
                move |data: &mut [i16], _| {
                    write_output_data(data, bit_depth, &generator, &lifecycle, &finished)
                },
                err_fn,
                None,
            )
            .map_err(|e| PlaybackError::BuildStream(e.to_string())),
        cpal::SampleFormat::U16 => device
            .build_output_stream(
                &config.config(),
                move |data: &mut [u16], _| {
                    write_output_data(data, bit_depth, &generator, &lifecycle, &finished)
                },
                err_fn,
                None,
            )
            .map_err(|e| PlaybackError::BuildStream(e.to_string())),
        _ => Err(PlaybackError::UnsupportedSampleFormat),
    }
}

fn write_output_data<T>(
    data: &mut [T],
    bit_depth: BitDepth,
    generator: &Arc<Mutex<PlaybackGenerator>>,
    lifecycle: &Arc<PlaybackLifecycle>,
    finished: &Arc<AtomicBool>,
) where
    T: cpal::Sample + cpal::FromSample<f32>,
{
    lifecycle.mark_started();

    let channels = 2usize;
    let frame_count = data.len() / channels;
    let bytes_per_frame = channels * bit_depth.bytes_per_sample();
    let mut pcm = vec![0u8; frame_count * bytes_per_frame];

    let written = {
        let mut guard = generator.lock().expect("generator mutex poisoned");
        guard.fill_pcm_bytes(&mut pcm)
    };

    let frames_written = written / bytes_per_frame;
    for frame in 0..frame_count {
        for ch in 0..channels {
            let sample = if frame < frames_written {
                decode_sample(&pcm, frame, ch, bit_depth)
            } else {
                0.0
            };
            data[frame * channels + ch] = T::from_sample(sample);
        }
    }

    if frames_written < frame_count {
        finished.store(true, Ordering::SeqCst);
    }
}

fn decode_sample(bytes: &[u8], frame: usize, channel: usize, bit_depth: BitDepth) -> f32 {
    match bit_depth {
        BitDepth::Pcm16 => {
            let offset = frame * 4 + channel * 2;
            let sample = i16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
            sample as f32 / i16::MAX as f32
        }
        BitDepth::Pcm24 => {
            let offset = frame * 6 + channel * 3;
            let sign = if bytes[offset + 2] & 0x80 != 0 {
                0xff
            } else {
                0x00
            };
            let sample =
                i32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], sign]);
            sample as f32 / 8_388_607.0
        }
    }
}
