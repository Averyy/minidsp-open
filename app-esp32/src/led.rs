use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::rmt::config::{MemoryAccess, TransmitConfig, TxChannelConfig};
use esp_idf_hal::rmt::encoder::{BytesEncoder, BytesEncoderConfig};
use esp_idf_hal::rmt::{PinState, Pulse, Symbol, TxChannelDriver};
use esp_idf_hal::units::Hertz;
use esp_idf_sys::EspError;
use log::info;
use std::thread;
use std::time::{Duration, Instant};

const RMT_RESOLUTION_HZ: Hertz = Hertz(10_000_000);
const DEMO_HOLD: Duration = Duration::from_secs(10);
const TRANSITION_GAP: Duration = Duration::from_millis(250);
const BREATH_STEP: Duration = Duration::from_millis(100);
const STATE_BREATHE_PERIOD: Duration = Duration::from_millis(4500);
const STATE_BREATHE_ENDPOINT_HOLD: Duration = Duration::from_millis(180);
const WS2812_T0H: Duration = Duration::from_nanos(350);
const WS2812_T0L: Duration = Duration::from_nanos(800);
const WS2812_T1H: Duration = Duration::from_nanos(700);
const WS2812_T1L: Duration = Duration::from_nanos(600);

#[derive(Clone, Copy, Debug)]
pub enum LedState {
    Booting,
    ProvisioningAp,
    WifiConnecting,
    MiniDspUnavailable,
    MiniDspReady,
    BackgroundUpdate,
    Fatal,
}

#[derive(Clone, Copy, Debug)]
struct Rgb {
    red: u8,
    green: u8,
    blue: u8,
}

impl Rgb {
    const BLACK: Self = Self::new(0, 0, 0);
    const RED: Self = Self::new(28, 0, 0);
    const GREEN: Self = Self::new(0, 24, 0);
    const BLUE: Self = Self::new(0, 0, 28);
    const AMBER: Self = Self::new(30, 13, 0);
    const AMBER_MIN: Self = Self::new(4, 2, 0);
    const WHITE: Self = Self::new(20, 20, 20);

    const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    fn scale(self, factor: f32) -> Self {
        let factor = factor.clamp(0.0, 1.0);
        if factor <= 0.0 {
            return Self::BLACK;
        }
        Self {
            red: scale_channel(self.red, factor),
            green: scale_channel(self.green, factor),
            blue: scale_channel(self.blue, factor),
        }
    }

    fn as_grb(self) -> [u8; 3] {
        [self.green, self.red, self.blue]
    }

    fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            red: lerp_channel(self.red, other.red, t),
            green: lerp_channel(self.green, other.green, t),
            blue: lerp_channel(self.blue, other.blue, t),
        }
    }
}

fn scale_channel(value: u8, factor: f32) -> u8 {
    if value == 0 {
        return 0;
    }
    (((value as f32) * factor).round() as u8).max(1)
}

fn lerp_channel(start: u8, end: u8, t: f32) -> u8 {
    ((start as f32) + ((end as f32) - (start as f32)) * t).round() as u8
}

pub struct Led<'d> {
    tx: TxChannelDriver<'d>,
    encoder: BytesEncoder,
    tx_config: TransmitConfig,
}

impl Led<'static> {
    pub fn new() -> Result<Self, EspError> {
        let peripherals = Peripherals::take()?;
        let tx = TxChannelDriver::new(
            peripherals.pins.gpio48,
            &TxChannelConfig {
                resolution: RMT_RESOLUTION_HZ,
                memory_access: MemoryAccess::Indirect {
                    memory_block_symbols: 64,
                },
                transaction_queue_depth: 1,
                ..Default::default()
            },
        )?;

        let bit0 = Symbol::new(
            Pulse::new_with_duration(RMT_RESOLUTION_HZ, PinState::High, WS2812_T0H)?,
            Pulse::new_with_duration(RMT_RESOLUTION_HZ, PinState::Low, WS2812_T0L)?,
        );
        let bit1 = Symbol::new(
            Pulse::new_with_duration(RMT_RESOLUTION_HZ, PinState::High, WS2812_T1H)?,
            Pulse::new_with_duration(RMT_RESOLUTION_HZ, PinState::Low, WS2812_T1L)?,
        );
        let encoder = BytesEncoder::with_config(&BytesEncoderConfig {
            bit0,
            bit1,
            msb_first: true,
            ..Default::default()
        })?;

        Ok(Self {
            tx,
            encoder,
            tx_config: TransmitConfig::default(),
        })
    }

    pub fn run_demo_loop(&mut self) -> Result<(), EspError> {
        info!("LED demo enabled on GPIO48");
        loop {
            self.run_color_verification()?;
            self.run_state_demo()?;
        }
    }

    pub fn run_first_color_test_loop(&mut self) -> Result<(), EspError> {
        info!("LED first-color test enabled on GPIO48");
        loop {
            self.show_color("raw red", Rgb::RED, DEMO_HOLD)?;
        }
    }

    pub fn run_color_verification_loop(&mut self) -> Result<(), EspError> {
        info!("LED color verification loop enabled on GPIO48");
        loop {
            self.run_color_verification()?;
        }
    }

    pub fn run_state_demo_loop(&mut self) -> Result<(), EspError> {
        info!("LED state demo loop enabled on GPIO48");
        loop {
            self.run_state_demo()?;
        }
    }

    pub fn run_amber_breathe_demo_loop(&mut self) -> Result<(), EspError> {
        info!("LED amber breathe demo loop enabled on GPIO48");
        loop {
            self.breathe_between(
                Rgb::AMBER_MIN,
                Rgb::AMBER,
                STATE_BREATHE_PERIOD,
                STATE_BREATHE_PERIOD,
            )?;
        }
    }
}

impl<'d> Led<'d> {
    fn run_color_verification(&mut self) -> Result<(), EspError> {
        self.show_color("raw red", Rgb::RED, DEMO_HOLD)?;
        self.show_color("raw green", Rgb::GREEN, DEMO_HOLD)?;
        self.show_color("raw blue", Rgb::BLUE, DEMO_HOLD)?;
        self.show_color("raw amber", Rgb::AMBER, DEMO_HOLD)?;
        self.show_color("raw white", Rgb::WHITE, DEMO_HOLD)?;
        self.show_color("raw off", Rgb::BLACK, DEMO_HOLD)?;
        Ok(())
    }

    fn run_state_demo(&mut self) -> Result<(), EspError> {
        self.show_state_for(LedState::Booting, DEMO_HOLD)?;
        self.show_state_for(LedState::ProvisioningAp, DEMO_HOLD)?;
        self.show_state_for(LedState::WifiConnecting, DEMO_HOLD)?;
        self.show_state_for(LedState::MiniDspUnavailable, DEMO_HOLD)?;
        self.show_state_for(LedState::MiniDspReady, DEMO_HOLD)?;
        self.show_state_for(LedState::BackgroundUpdate, DEMO_HOLD)?;
        self.show_state_for(LedState::Fatal, DEMO_HOLD)?;
        Ok(())
    }

    fn show_state_for(&mut self, state: LedState, duration: Duration) -> Result<(), EspError> {
        info!("LED state demo: {state:?}");
        match state {
            LedState::Booting => self.hold(Rgb::AMBER, duration),
            LedState::ProvisioningAp => {
                self.breathe(Rgb::BLUE, duration, STATE_BREATHE_PERIOD, 0.0, 1.0)
            }
            LedState::WifiConnecting => self.flash(
                Rgb::BLUE,
                duration,
                Duration::from_millis(150),
                Duration::from_millis(150),
            ),
            LedState::MiniDspUnavailable => {
                self.breathe_between(Rgb::AMBER_MIN, Rgb::AMBER, duration, STATE_BREATHE_PERIOD)
            }
            LedState::MiniDspReady => self.hold(Rgb::GREEN.scale(0.6), duration),
            LedState::BackgroundUpdate => {
                self.breathe(Rgb::GREEN, duration, STATE_BREATHE_PERIOD, 0.0, 1.0)
            }
            LedState::Fatal => self.hold(Rgb::RED, duration),
        }?;
        self.off()?;
        thread::sleep(TRANSITION_GAP);
        Ok(())
    }

    fn show_color(&mut self, label: &str, color: Rgb, duration: Duration) -> Result<(), EspError> {
        info!("LED color demo: {label}");
        self.write(color)?;
        thread::sleep(duration);
        self.off()?;
        thread::sleep(TRANSITION_GAP);
        Ok(())
    }

    fn hold(&mut self, color: Rgb, duration: Duration) -> Result<(), EspError> {
        self.write(color)?;
        thread::sleep(duration);
        Ok(())
    }

    fn flash(
        &mut self,
        color: Rgb,
        duration: Duration,
        on_time: Duration,
        off_time: Duration,
    ) -> Result<(), EspError> {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.write(color)?;
            thread::sleep(on_time.min(deadline.saturating_duration_since(Instant::now())));
            if Instant::now() >= deadline {
                break;
            }
            self.off()?;
            thread::sleep(off_time.min(deadline.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }

    fn breathe(
        &mut self,
        color: Rgb,
        duration: Duration,
        period: Duration,
        min_factor: f32,
        max_factor: f32,
    ) -> Result<(), EspError> {
        let deadline = Instant::now() + duration;
        let period_ms = period.as_millis().max(1) as f32;
        let endpoint_hold_ms = STATE_BREATHE_ENDPOINT_HOLD.as_millis() as f32;
        let ramp_ms = ((period_ms - (endpoint_hold_ms * 2.0)) / 2.0).max(1.0);
        while Instant::now() < deadline {
            let elapsed = duration
                .saturating_sub(deadline.saturating_duration_since(Instant::now()))
                .as_millis() as f32;
            let phase_ms = elapsed % period_ms;
            let wave = if phase_ms < endpoint_hold_ms {
                0.0
            } else if phase_ms < endpoint_hold_ms + ramp_ms {
                (phase_ms - endpoint_hold_ms) / ramp_ms
            } else if phase_ms < endpoint_hold_ms + ramp_ms + endpoint_hold_ms {
                1.0
            } else {
                1.0 - ((phase_ms - endpoint_hold_ms - ramp_ms - endpoint_hold_ms) / ramp_ms)
            };
            let factor = min_factor + (max_factor - min_factor) * wave;
            self.write(color.scale(factor))?;
            thread::sleep(BREATH_STEP.min(deadline.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }

    fn breathe_between(
        &mut self,
        low: Rgb,
        high: Rgb,
        duration: Duration,
        period: Duration,
    ) -> Result<(), EspError> {
        let deadline = Instant::now() + duration;
        let period_ms = period.as_millis().max(1) as f32;
        let endpoint_hold_ms = STATE_BREATHE_ENDPOINT_HOLD.as_millis() as f32;
        let ramp_ms = ((period_ms - (endpoint_hold_ms * 2.0)) / 2.0).max(1.0);
        while Instant::now() < deadline {
            let elapsed = duration
                .saturating_sub(deadline.saturating_duration_since(Instant::now()))
                .as_millis() as f32;
            let phase_ms = elapsed % period_ms;
            let wave = if phase_ms < endpoint_hold_ms {
                0.0
            } else if phase_ms < endpoint_hold_ms + ramp_ms {
                (phase_ms - endpoint_hold_ms) / ramp_ms
            } else if phase_ms < endpoint_hold_ms + ramp_ms + endpoint_hold_ms {
                1.0
            } else {
                1.0 - ((phase_ms - endpoint_hold_ms - ramp_ms - endpoint_hold_ms) / ramp_ms)
            };
            self.write(low.lerp(high, wave))?;
            thread::sleep(BREATH_STEP.min(deadline.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }

    fn off(&mut self) -> Result<(), EspError> {
        self.write(Rgb::BLACK)
    }

    fn write(&mut self, color: Rgb) -> Result<(), EspError> {
        let bytes = color.as_grb();
        self.tx
            .send_and_wait(&mut self.encoder, &bytes, &self.tx_config)
    }
}
