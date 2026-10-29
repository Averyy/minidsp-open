//! Desktop audio playback backend for `/play`.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use minidsp_protocol::playback::{
    BitDepth, PlayResponse, PlaybackGenerator, PlaybackPlan, SampleLayout,
};
use minidsp_protocol::rattle::{RattleGenerator, RattlePlan, RattleResponse};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

const START_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("cannot identify the controlled miniDSP audio output unambiguously: {0}")]
    OutputIdentity(String),
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

/// `on_started` runs once the stream has started, before the response is
/// sent on `started_tx`.
pub fn run_playback(
    device: cpal::Device,
    plan: PlaybackPlan,
    source_changed: bool,
    previous_source: Option<String>,
    on_started: impl FnOnce(&PlayResponse),
    started_tx: oneshot::Sender<Result<PlayResponse, String>>,
) -> Result<(), PlaybackError> {
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

    let mut started_tx = Some(started_tx);
    let result = run_pcm(
        device,
        layout,
        Arc::new(Mutex::new(plan.generator())),
        Arc::new(AtomicI64::new(0)),
        |started_at_us| {
            let response = PlayResponse {
                started_at_us,
                ..response
            };
            on_started(&response);
            let _ = started_tx.take().unwrap().send(Ok(response));
        },
    );
    if let (Err(err), Some(tx)) = (&result, started_tx) {
        let _ = tx.send(Err(err.to_string()));
    }
    result
}

pub fn run_rattle(
    device: cpal::Device,
    plan: RattlePlan,
    generator: Arc<Mutex<RattleGenerator>>,
    output_latency_us: Arc<AtomicI64>,
    response: RattleResponse,
    on_started: impl FnOnce(&RattleResponse),
    started_tx: oneshot::Sender<Result<RattleResponse, String>>,
) -> Result<(), PlaybackError> {
    let mut started_tx = Some(started_tx);
    let result = run_pcm(
        device,
        plan.layout(),
        generator,
        output_latency_us,
        |started_at_us| {
            let response = RattleResponse {
                started_at_us,
                ..response
            };
            on_started(&response);
            let _ = started_tx.take().unwrap().send(Ok(response));
        },
    );
    if let (Err(err), Some(tx)) = (&result, started_tx) {
        let _ = tx.send(Err(err.to_string()));
    }
    result
}

trait PcmSource: Send + 'static {
    fn fill_pcm_bytes(&mut self, out: &mut [u8]) -> usize;
    fn is_finished(&self) -> bool;
}

macro_rules! pcm_source {
    ($generator:ty) => {
        impl PcmSource for $generator {
            fn fill_pcm_bytes(&mut self, out: &mut [u8]) -> usize {
                self.fill_pcm_bytes(out)
            }
            fn is_finished(&self) -> bool {
                self.is_finished()
            }
        }
    };
}
pcm_source!(PlaybackGenerator);
pcm_source!(RattleGenerator);

fn run_pcm<G: PcmSource>(
    device: cpal::Device,
    layout: SampleLayout,
    generator: Arc<Mutex<G>>,
    output_latency_us: Arc<AtomicI64>,
    on_started: impl FnOnce(i64),
) -> Result<(), PlaybackError> {
    let stream_config = select_stream_config(&device, layout.sample_rate)?;
    let lifecycle = Arc::new(PlaybackLifecycle {
        output_latency_us,
        ..PlaybackLifecycle::default()
    });
    let finished = Arc::new(AtomicBool::new(false));
    let stream = build_stream(
        &device,
        &stream_config,
        layout.bit_depth,
        generator,
        lifecycle.clone(),
        finished.clone(),
    )?;
    stream
        .play()
        .map_err(|e| PlaybackError::StartStream(e.to_string()))?;
    on_started(lifecycle.wait_for_start()?);
    while !finished.load(Ordering::SeqCst) {
        if let Some(err) = lifecycle.take_error() {
            return Err(PlaybackError::StreamError(err));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(err) = lifecycle.take_error() {
        return Err(PlaybackError::StreamError(err));
    }
    // Generator completion only means the last samples were queued. Keep the
    // stream alive until their callback timestamp plus device latency elapses.
    let drain_deadline = *lifecycle
        .drain_deadline
        .lock()
        .expect("drain mutex poisoned");
    if let Some(deadline) = drain_deadline {
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            if let Some(err) = lifecycle.take_error() {
                return Err(PlaybackError::StreamError(err));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
    if let Some(err) = lifecycle.take_error() {
        return Err(PlaybackError::StreamError(err));
    }
    drop(stream);

    Ok(())
}

#[derive(Default)]
struct PlaybackLifecycle {
    output_latency_us: Arc<AtomicI64>,
    started_at_us: Mutex<Option<i64>>,
    error: Mutex<Option<String>>,
    drain_deadline: Mutex<Option<Instant>>,
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

/// Resolve before any source/volume write, then pass this exact device to CPAL.
/// CPAL 0.15 exposes names but no portable USB identity. Refuse multiple local
/// controllers rather than assuming HID and audio enumeration have the same order.
pub(crate) fn select_output_device(
    client: &minidsp::client::MiniDspClient,
    local_audio_devices: usize,
) -> Result<cpal::Device, PlaybackError> {
    let host = cpal::default_host();
    let devices: Vec<_> = host
        .output_devices()
        .map_err(|e| PlaybackError::EnumerateDevices(e.to_string()))?
        .collect();
    let names = devices
        .iter()
        .map(|device| {
            device
                .name()
                .map_err(|e| PlaybackError::DeviceName(e.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let index = matching_output_index(
        &names,
        &client.device_info().product_name,
        client.transport_url().starts_with("usb:"),
        local_audio_devices,
        std::env::var("MINIDSP_PLAY_DEVICE").ok().as_deref(),
    )?;
    Ok(devices
        .into_iter()
        .nth(index)
        .expect("validated output index"))
}

fn matching_output_index(
    names: &[String],
    product: &str,
    local: bool,
    local_audio_devices: usize,
    hint: Option<&str>,
) -> Result<usize, PlaybackError> {
    let fail = || {
        PlaybackError::OutputIdentity(
        "requires one local USB audio controller and one matching miniDSP output; network devices and multiple miniDSPs cannot be safely mapped by CPAL names".into())
    };
    if !local || local_audio_devices != 1 {
        return Err(fail());
    }
    let outputs: Vec<_> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.to_lowercase().contains("minidsp"))
        .collect();
    if outputs.len() != 1 {
        return Err(fail());
    }
    let (index, name) = outputs[0];
    let normalize = |value: &str| {
        value
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let product = normalize(product);
    let product = product.strip_prefix("minidsp").unwrap_or(&product);
    if product.is_empty()
        || !normalize(name).contains(product)
        || hint.is_some_and(|hint| {
            hint.is_empty() || !name.to_lowercase().contains(&hint.to_lowercase())
        })
    {
        return Err(fail());
    }
    Ok(index)
}

fn select_stream_config(
    device: &cpal::Device,
    sample_rate: u32,
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
        if sample_rate < min || sample_rate > max {
            continue;
        }
        let exact = cfg.with_sample_rate(cpal::SampleRate(sample_rate));
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
            sample_rate
        ))
    })
}

fn build_stream<G: PcmSource>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    bit_depth: BitDepth,
    generator: Arc<Mutex<G>>,
    lifecycle: Arc<PlaybackLifecycle>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, PlaybackError> {
    let sample_rate = config.sample_rate().0;
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
                move |data: &mut [f32], info| {
                    write_output_data(
                        data,
                        bit_depth,
                        &generator,
                        &lifecycle,
                        &finished,
                        info,
                        sample_rate,
                    )
                },
                err_fn,
                None,
            )
            .map_err(|e| PlaybackError::BuildStream(e.to_string())),
        cpal::SampleFormat::I16 => device
            .build_output_stream(
                &config.config(),
                move |data: &mut [i16], info| {
                    write_output_data(
                        data,
                        bit_depth,
                        &generator,
                        &lifecycle,
                        &finished,
                        info,
                        sample_rate,
                    )
                },
                err_fn,
                None,
            )
            .map_err(|e| PlaybackError::BuildStream(e.to_string())),
        cpal::SampleFormat::U16 => device
            .build_output_stream(
                &config.config(),
                move |data: &mut [u16], info| {
                    write_output_data(
                        data,
                        bit_depth,
                        &generator,
                        &lifecycle,
                        &finished,
                        info,
                        sample_rate,
                    )
                },
                err_fn,
                None,
            )
            .map_err(|e| PlaybackError::BuildStream(e.to_string())),
        _ => Err(PlaybackError::UnsupportedSampleFormat),
    }
}

fn write_output_data<T, G: PcmSource>(
    data: &mut [T],
    bit_depth: BitDepth,
    generator: &Arc<Mutex<G>>,
    lifecycle: &Arc<PlaybackLifecycle>,
    finished: &Arc<AtomicBool>,
    info: &cpal::OutputCallbackInfo,
    sample_rate: u32,
) where
    T: cpal::Sample + cpal::FromSample<f32>,
{
    let callback_at = Instant::now();
    let timestamp = info.timestamp();
    let latency = timestamp
        .playback
        .duration_since(&timestamp.callback)
        .unwrap_or_default();
    lifecycle.output_latency_us.store(
        latency.as_micros().min(i64::MAX as u128) as i64,
        Ordering::SeqCst,
    );
    lifecycle.mark_started();

    let channels = 2usize;
    let frame_count = data.len() / channels;
    let bytes_per_frame = channels * bit_depth.bytes_per_sample();
    let mut pcm = vec![0u8; frame_count * bytes_per_frame];

    let (written, source_finished) = {
        let mut guard = generator.lock().expect("generator mutex poisoned");
        let written = guard.fill_pcm_bytes(&mut pcm);
        (written, guard.is_finished())
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

    if frames_written > 0 {
        *lifecycle
            .drain_deadline
            .lock()
            .expect("drain mutex poisoned") =
            Some(callback_at + output_drain_duration(latency, frames_written, sample_rate));
    }
    if frames_written < frame_count && source_finished {
        finished.store(true, Ordering::SeqCst);
    }
}

fn output_drain_duration(latency: Duration, frames: usize, sample_rate: u32) -> Duration {
    latency + Duration::from_secs_f64(frames as f64 / sample_rate as f64)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_identity_refuses_wrong_ambiguous_and_remote_devices() {
        let names = vec!["Built-in speakers".into(), "miniDSP 2x4 HD".into()];
        let select = |names: &[String], local, count, hint| {
            matching_output_index(names, "2x4 HD", local, count, hint)
        };
        assert_eq!(select(&names, true, 1, None).unwrap(), 1);
        assert!(select(&names, true, 2, None).is_err());
        assert!(select(&names, false, 1, None).is_err());
        assert!(select(&names, true, 0, None).is_err());
        assert!(select(&["miniDSP Flex".into()], true, 1, None).is_err());
        assert!(
            select(
                &["miniDSP 2x4 HD".into(), "miniDSP 2x4 HD".into()],
                true,
                1,
                None
            )
            .is_err()
        );
        assert!(select(&names, true, 1, Some("Built-in")).is_err());
        assert!(select(&names, true, 2, Some("2x4")).is_err());
        assert_eq!(select(&names, true, 1, Some("2x4")).unwrap(), 1);
    }

    #[test]
    fn playback_drain_includes_device_latency_and_final_samples() {
        assert_eq!(
            output_drain_duration(Duration::from_millis(100), 4800, 48000),
            Duration::from_millis(200)
        );
        assert_eq!(
            output_drain_duration(Duration::from_millis(5), 480, 48000),
            Duration::from_millis(15)
        );
    }
}
