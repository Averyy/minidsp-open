#[path = "../led.rs"]
#[allow(dead_code)]
mod led;
#[path = "../led_demo.rs"]
#[allow(dead_code)]
mod led_demo;
#[path = "../threads.rs"]
#[allow(dead_code)]
mod threads;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    match led::Led::new() {
        Ok(mut led) => {
            if let Err(e) = led_demo::run_amber_breathe_demo_loop(&mut led) {
                log::error!("LED demo failed: {e}");
            }
        }
        Err(e) => log::error!("LED demo init failed: {e}"),
    }

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
