use esp_idf_hal::peripherals::Peripherals;
use esp_idf_hal::rmt::config::{MemoryAccess, TransmitConfig, TxChannelConfig};
use esp_idf_hal::rmt::encoder::{BytesEncoder, BytesEncoderConfig};
use esp_idf_hal::rmt::{PinState, Pulse, Symbol, TxChannelDriver};
use esp_idf_hal::units::Hertz;
use esp_idf_sys::EspError;
use log::{error, info};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

const RMT_RESOLUTION_HZ: Hertz = Hertz(10_000_000);
const DEMO_HOLD: Duration = Duration::from_secs(10);
const TRANSITION_GAP: Duration = Duration::from_millis(250);
const BREATH_STEP: Duration = Duration::from_millis(100);
const FLASH_ON_TIME: Duration = Duration::from_millis(150);
const FLASH_OFF_TIME: Duration = Duration::from_millis(150);
const LED_THREAD_STACK_BYTES: usize = 8 * 1024;
const STATE_BREATHE_PERIOD: Duration = Duration::from_millis(4500);
const STATE_BREATHE_ENDPOINT_HOLD: Duration = Duration::from_millis(180);
const WS2812_T0H: Duration = Duration::from_nanos(350);
const WS2812_T0L: Duration = Duration::from_nanos(800);
const WS2812_T1H: Duration = Duration::from_nanos(700);
const WS2812_T1L: Duration = Duration::from_nanos(600);

static GLOBAL_CONTROLLER: OnceLock<LedController> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedMode {
    Booting,
    ProvisioningAp,
    WifiConnecting,
    Operational,
    Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedState {
    Booting,
    ProvisioningAp,
    WifiConnecting,
    MiniDspUnavailable,
    MiniDspReady,
    BackgroundUpdate,
    Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone)]
pub struct LedController {
    tx: Sender<LedCommand>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LedStatus {
    mode: LedMode,
    device_ready: bool,
    background_update: bool,
}

#[derive(Clone, Copy, Debug)]
enum LedCommand {
    SetMode(LedMode),
    SetDeviceReady(bool),
    SetBackgroundUpdate(bool),
}

pub struct BackgroundUpdateGuard;

impl Default for LedStatus {
    fn default() -> Self {
        Self {
            mode: LedMode::Booting,
            device_ready: false,
            background_update: false,
        }
    }
}

impl LedStatus {
    fn resolve(self) -> LedState {
        match self.mode {
            LedMode::Fatal => LedState::Fatal,
            LedMode::Booting => LedState::Booting,
            LedMode::ProvisioningAp => LedState::ProvisioningAp,
            LedMode::WifiConnecting => LedState::WifiConnecting,
            LedMode::Operational => {
                if !self.device_ready {
                    LedState::MiniDspUnavailable
                } else if self.background_update {
                    LedState::BackgroundUpdate
                } else {
                    LedState::MiniDspReady
                }
            }
        }
    }
}

impl LedController {
    fn spawn() -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("rgb_led".into())
            .stack_size(LED_THREAD_STACK_BYTES)
            .spawn(move || match Led::new() {
                Ok(led) => {
                    let _ = ready_tx.send(Ok(()));
                    run_led_controller(led, rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("RGB LED init failed: {e}")));
                }
            })
            .map_err(|e| format!("RGB LED thread spawn failed: {e}"))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self { tx }),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("RGB LED thread did not report readiness: {e}")),
        }
    }

    pub fn set_mode(&self, mode: LedMode) {
        let _ = self.tx.send(LedCommand::SetMode(mode));
    }

    pub fn set_device_ready(&self, ready: bool) {
        let _ = self.tx.send(LedCommand::SetDeviceReady(ready));
    }

    pub fn set_background_update(&self, active: bool) {
        let _ = self.tx.send(LedCommand::SetBackgroundUpdate(active));
    }
}

impl Drop for BackgroundUpdateGuard {
    fn drop(&mut self) {
        set_background_update(false);
    }
}

pub fn start_global_controller() -> Result<LedController, String> {
    if let Some(controller) = GLOBAL_CONTROLLER.get() {
        return Ok(controller.clone());
    }
    let controller = LedController::spawn()?;
    let _ = GLOBAL_CONTROLLER.set(controller.clone());
    Ok(controller)
}

pub fn set_mode(mode: LedMode) {
    if let Some(controller) = GLOBAL_CONTROLLER.get() {
        controller.set_mode(mode);
    }
}

pub fn set_device_ready(ready: bool) {
    if let Some(controller) = GLOBAL_CONTROLLER.get() {
        controller.set_device_ready(ready);
    }
}

pub fn set_background_update(active: bool) {
    if let Some(controller) = GLOBAL_CONTROLLER.get() {
        controller.set_background_update(active);
    }
}

pub fn begin_background_update() -> BackgroundUpdateGuard {
    set_background_update(true);
    BackgroundUpdateGuard
}

pub struct Led<'d> {
    tx: TxChannelDriver<'d>,
    encoder: BytesEncoder,
    tx_config: TransmitConfig,
    last_color: Option<Rgb>,
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
            last_color: None,
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
    fn render_state_frame(&mut self, state: LedState, elapsed: Duration) -> Result<(), EspError> {
        self.write(Self::color_for_state(state, elapsed))
    }

    fn color_for_state(state: LedState, elapsed: Duration) -> Rgb {
        match state {
            LedState::Booting => Rgb::AMBER,
            LedState::ProvisioningAp => {
                Self::breathe_color(Rgb::BLUE, elapsed, STATE_BREATHE_PERIOD, 0.0, 1.0)
            }
            LedState::WifiConnecting => {
                Self::flash_color(Rgb::BLUE, elapsed, FLASH_ON_TIME, FLASH_OFF_TIME)
            }
            LedState::MiniDspUnavailable => Self::breathe_between_color(
                Rgb::AMBER_MIN,
                Rgb::AMBER,
                elapsed,
                STATE_BREATHE_PERIOD,
            ),
            LedState::MiniDspReady => Rgb::GREEN.scale(0.6),
            LedState::BackgroundUpdate => {
                Self::breathe_color(Rgb::GREEN, elapsed, STATE_BREATHE_PERIOD, 0.0, 1.0)
            }
            LedState::Fatal => Rgb::RED,
        }
    }

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
        self.run_animation(duration, |elapsed| Self::color_for_state(state, elapsed))?;
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
        self.run_animation(duration, |_| color)
    }

    fn flash(
        &mut self,
        color: Rgb,
        duration: Duration,
        on_time: Duration,
        off_time: Duration,
    ) -> Result<(), EspError> {
        self.run_animation(duration, |elapsed| Self::flash_color(color, elapsed, on_time, off_time))
    }

    fn breathe(
        &mut self,
        color: Rgb,
        duration: Duration,
        period: Duration,
        min_factor: f32,
        max_factor: f32,
    ) -> Result<(), EspError> {
        self.run_animation(duration, |elapsed| {
            Self::breathe_color(color, elapsed, period, min_factor, max_factor)
        })
    }

    fn breathe_between(
        &mut self,
        low: Rgb,
        high: Rgb,
        duration: Duration,
        period: Duration,
    ) -> Result<(), EspError> {
        self.run_animation(duration, |elapsed| {
            Self::breathe_between_color(low, high, elapsed, period)
        })
    }

    fn run_animation<F>(&mut self, duration: Duration, mut frame: F) -> Result<(), EspError>
    where
        F: FnMut(Duration) -> Rgb,
    {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            let elapsed = duration
                .saturating_sub(deadline.saturating_duration_since(Instant::now()));
            self.write(frame(elapsed))?;
            thread::sleep(BREATH_STEP.min(deadline.saturating_duration_since(Instant::now())));
        }
        Ok(())
    }

    fn off(&mut self) -> Result<(), EspError> {
        self.write(Rgb::BLACK)
    }

    fn write(&mut self, color: Rgb) -> Result<(), EspError> {
        if self.last_color == Some(color) {
            return Ok(());
        }
        let bytes = color.as_grb();
        self.tx
            .send_and_wait(&mut self.encoder, &bytes, &self.tx_config)?;
        self.last_color = Some(color);
        Ok(())
    }

    fn flash_color(color: Rgb, elapsed: Duration, on_time: Duration, off_time: Duration) -> Rgb {
        let cycle_ms = (on_time + off_time).as_millis().max(1) as u128;
        if elapsed.as_millis() % cycle_ms < on_time.as_millis() as u128 {
            color
        } else {
            Rgb::BLACK
        }
    }

    fn breathe_color(
        color: Rgb,
        elapsed: Duration,
        period: Duration,
        min_factor: f32,
        max_factor: f32,
    ) -> Rgb {
        let wave = Self::breathe_wave(elapsed, period);
        let factor = min_factor + (max_factor - min_factor) * wave;
        color.scale(factor)
    }

    fn breathe_between_color(low: Rgb, high: Rgb, elapsed: Duration, period: Duration) -> Rgb {
        low.lerp(high, Self::breathe_wave(elapsed, period))
    }

    fn breathe_wave(elapsed: Duration, period: Duration) -> f32 {
        let period_ms = period.as_millis().max(1) as f32;
        let endpoint_hold_ms = STATE_BREATHE_ENDPOINT_HOLD.as_millis() as f32;
        let ramp_ms = ((period_ms - (endpoint_hold_ms * 2.0)) / 2.0).max(1.0);
        let phase_ms = elapsed.as_millis() as f32 % period_ms;
        if phase_ms < endpoint_hold_ms {
            0.0
        } else if phase_ms < endpoint_hold_ms + ramp_ms {
            (phase_ms - endpoint_hold_ms) / ramp_ms
        } else if phase_ms < endpoint_hold_ms + ramp_ms + endpoint_hold_ms {
            1.0
        } else {
            1.0 - ((phase_ms - endpoint_hold_ms - ramp_ms - endpoint_hold_ms) / ramp_ms)
        }
    }
}

fn run_led_controller(mut led: Led<'static>, rx: Receiver<LedCommand>) {
    let mut status = LedStatus::default();
    let mut active_state = status.resolve();
    let mut active_since = Instant::now();

    info!("LED controller active on GPIO48");
    if let Err(e) = led.render_state_frame(active_state, Duration::ZERO) {
        error!("LED initial write failed: {e}");
    }

    loop {
        while let Ok(command) = rx.try_recv() {
            apply_command(&mut status, command);
        }

        let resolved = status.resolve();
        if resolved != active_state {
            info!("LED state: {active_state:?} -> {resolved:?}");
            active_state = resolved;
            active_since = Instant::now();
        }

        let elapsed = Instant::now().saturating_duration_since(active_since);
        if let Err(e) = led.render_state_frame(active_state, elapsed) {
            error!("LED render failed: {e}");
            thread::sleep(Duration::from_secs(1));
        }

        match rx.recv_timeout(BREATH_STEP) {
            Ok(command) => apply_command(&mut status, command),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = led.off();
                break;
            }
        }
    }
}

fn apply_command(status: &mut LedStatus, command: LedCommand) {
    match command {
        LedCommand::SetMode(LedMode::Fatal) => status.mode = LedMode::Fatal,
        LedCommand::SetMode(mode) => {
            if status.mode != LedMode::Fatal {
                status.mode = mode;
            }
        }
        LedCommand::SetDeviceReady(ready) => status.device_ready = ready,
        LedCommand::SetBackgroundUpdate(active) => status.background_update = active,
    }
}
