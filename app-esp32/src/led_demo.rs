use crate::led::{Led, LedState, Rgb};
use esp_idf_sys::EspError;
use log::info;
use std::thread;
use std::time::{Duration, Instant};

const DEMO_HOLD: Duration = Duration::from_secs(10);
const TRANSITION_GAP: Duration = Duration::from_millis(250);
const DEMO_FRAME_STEP: Duration = Duration::from_millis(100);
const WHITE: Rgb = Rgb::new(20, 20, 20);

pub fn run_demo_loop(led: &mut Led<'_>) -> Result<(), EspError> {
    info!("LED demo enabled on GPIO48");
    loop {
        run_color_verification(led)?;
        run_state_demo(led)?;
    }
}

pub fn run_first_color_test_loop(led: &mut Led<'_>) -> Result<(), EspError> {
    info!("LED first-color test enabled on GPIO48");
    loop {
        show_color(led, "raw red", Rgb::RED, DEMO_HOLD)?;
    }
}

pub fn run_color_verification_loop(led: &mut Led<'_>) -> Result<(), EspError> {
    info!("LED color verification loop enabled on GPIO48");
    loop {
        run_color_verification(led)?;
    }
}

pub fn run_state_demo_loop(led: &mut Led<'_>) -> Result<(), EspError> {
    info!("LED state demo loop enabled on GPIO48");
    loop {
        run_state_demo(led)?;
    }
}

pub fn run_amber_breathe_demo_loop(led: &mut Led<'_>) -> Result<(), EspError> {
    info!("LED amber breathe demo enabled on GPIO48");
    loop {
        run_animation(led, DEMO_HOLD, |led, elapsed| {
            led.render_state_frame(LedState::MiniDspUnavailable, elapsed)
        })?;
    }
}

fn run_color_verification(led: &mut Led<'_>) -> Result<(), EspError> {
    show_color(led, "raw red", Rgb::RED, DEMO_HOLD)?;
    show_color(led, "raw green", Rgb::GREEN, DEMO_HOLD)?;
    show_color(led, "raw blue", Rgb::BLUE, DEMO_HOLD)?;
    show_color(led, "raw amber", Rgb::AMBER, DEMO_HOLD)?;
    show_color(led, "raw white", WHITE, DEMO_HOLD)?;
    show_color(led, "raw off", Rgb::BLACK, DEMO_HOLD)?;
    Ok(())
}

fn run_state_demo(led: &mut Led<'_>) -> Result<(), EspError> {
    show_state_for(led, LedState::Booting, DEMO_HOLD)?;
    show_state_for(led, LedState::ProvisioningAp, DEMO_HOLD)?;
    show_state_for(led, LedState::WifiConnecting, DEMO_HOLD)?;
    show_state_for(led, LedState::MiniDspUnavailable, DEMO_HOLD)?;
    show_state_for(led, LedState::MiniDspReady, DEMO_HOLD)?;
    show_state_for(led, LedState::BackgroundUpdate, DEMO_HOLD)?;
    show_state_for(led, LedState::Fatal, DEMO_HOLD)?;
    Ok(())
}

fn show_state_for(led: &mut Led<'_>, state: LedState, duration: Duration) -> Result<(), EspError> {
    info!("LED state demo: {state:?}");
    run_animation(led, duration, |led, elapsed| {
        led.render_state_frame(state, elapsed)
    })?;
    led.off()?;
    thread::sleep(TRANSITION_GAP);
    Ok(())
}

fn show_color(
    led: &mut Led<'_>,
    label: &str,
    color: Rgb,
    duration: Duration,
) -> Result<(), EspError> {
    info!("LED color demo: {label}");
    run_animation(led, duration, |led, _| led.write(color))?;
    led.off()?;
    thread::sleep(TRANSITION_GAP);
    Ok(())
}

fn run_animation<F>(led: &mut Led<'_>, duration: Duration, mut frame: F) -> Result<(), EspError>
where
    F: FnMut(&mut Led<'_>, Duration) -> Result<(), EspError>,
{
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let elapsed = duration.saturating_sub(deadline.saturating_duration_since(Instant::now()));
        frame(led, elapsed)?;
        thread::sleep(DEMO_FRAME_STEP.min(deadline.saturating_duration_since(Instant::now())));
    }
    Ok(())
}
