//! Thread spawning helpers: PSRAM stacks, and long-lived workers for
//! recurring jobs.

use esp_idf_hal::task::thread::{MallocCap, ThreadSpawnConfiguration};
use log::warn;
use std::sync::{mpsc, Mutex};

fn copy_thread_spawn_configuration(conf: &ThreadSpawnConfiguration) -> ThreadSpawnConfiguration {
    ThreadSpawnConfiguration {
        name: conf.name,
        stack_size: conf.stack_size,
        priority: conf.priority,
        inherit: conf.inherit,
        pin_to_core: conf.pin_to_core,
        stack_alloc_caps: conf.stack_alloc_caps,
    }
}

/// Spawn a std thread whose stack lives in PSRAM. Only for threads that never
/// write flash (NVS, LittleFS, OTA): flash writes disable the cache, and a
/// task can't run on a PSRAM stack while it's off.
pub fn spawn_psram_thread<F>(
    conf_name: &'static std::ffi::CStr,
    builder_name: &str,
    stack_size: usize,
    f: F,
) -> Result<std::thread::JoinHandle<()>, String>
where
    F: FnOnce() + Send + 'static,
{
    let previous_conf = ThreadSpawnConfiguration::get();
    let mut spawn_conf = previous_conf
        .as_ref()
        .map(copy_thread_spawn_configuration)
        .unwrap_or_default();
    spawn_conf.name = Some(conf_name);
    spawn_conf.stack_size = stack_size;
    spawn_conf.stack_alloc_caps = MallocCap::Spiram | MallocCap::Cap8bit;
    spawn_conf
        .set()
        .map_err(|err| format!("set {builder_name} thread config: {err}"))?;

    let spawn_result = std::thread::Builder::new()
        .name(builder_name.into())
        .stack_size(stack_size)
        .spawn(f);

    let restore_result = previous_conf
        .as_ref()
        .map(ThreadSpawnConfiguration::set)
        .unwrap_or_else(|| ThreadSpawnConfiguration::default().set());
    if let Err(err) = restore_result {
        warn!("failed to restore pthread config after {builder_name} spawn: {err}");
    }

    spawn_result.map_err(|err| err.to_string())
}

type WorkerJob = Box<dyn FnOnce() + Send>;

/// A long-lived PSRAM-stack thread that runs submitted jobs in order.
///
/// On ESP-IDF every thread that exits leaks its std `Thread` handle, about
/// 180 B of internal RAM (found with a heap trace): std's deferred thread
/// cleanup reads the handle from a TLS key that IDF's pthread exit has
/// already freed, so the handle is never dropped. Recurring work (a playback
/// per `/play`, a WebSocket poller per connection) reuses one of these
/// instead of spawning a thread each time.
pub struct Worker {
    name: &'static std::ffi::CStr,
    label: &'static str,
    stack_size: usize,
    jobs: Mutex<Option<mpsc::Sender<WorkerJob>>>,
}

impl Worker {
    pub const fn new(
        name: &'static std::ffi::CStr,
        label: &'static str,
        stack_size: usize,
    ) -> Self {
        Self {
            name,
            label,
            stack_size,
            jobs: Mutex::new(None),
        }
    }

    /// Queue `job`, starting the thread on first use.
    pub fn run(&self, job: impl FnOnce() + Send + 'static) -> Result<(), String> {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let job: WorkerJob = Box::new(job);
        let job = match jobs.as_ref() {
            Some(tx) => match tx.send(job) {
                Ok(()) => return Ok(()),
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (tx, rx) = mpsc::channel::<WorkerJob>();
        tx.send(job)
            .map_err(|_| format!("{} worker queue closed", self.label))?;
        spawn_psram_thread(self.name, self.label, self.stack_size, move || {
            for job in rx {
                job();
            }
        })?;
        *jobs = Some(tx);
        Ok(())
    }
}
