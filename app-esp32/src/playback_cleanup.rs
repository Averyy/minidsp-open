//! Exit cleanup shared with host regression tests; hardware actions stay in the caller.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Once;

thread_local! {
    static PANIC_CLEANUP: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

/// ESP-IDF aborts on panic, so its hook must run cleanup before destructors
/// would run. The hook is process-wide, but the registered action is scoped
/// to this worker thread and removed on normal exit.
pub struct PanicCleanupRegistration;
impl Drop for PanicCleanupRegistration {
    fn drop(&mut self) {
        PANIC_CLEANUP.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}

#[derive(Clone)]
pub struct CleanupToken(Rc<Cell<bool>>);

impl CleanupToken {
    pub fn arm_on_panic<T: Cleanup + 'static>(&self, context: T) -> PanicCleanupRegistration {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let action = PANIC_CLEANUP.with(|slot| slot.borrow_mut().take());
                if let Some(action) = action {
                    action();
                }
                previous(info);
            }));
        });
        let token = self.clone();
        PANIC_CLEANUP.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let mut guard = CleanupGuard {
                    context,
                    finished: token,
                };
                guard.finish();
            }));
        });
        PanicCleanupRegistration
    }
}

/// Preserve stream errors and make failed teardown visible to both APIs.
pub fn teardown_error(error: Option<String>, recovery_error: String) -> Option<String> {
    let teardown = format!("USB audio stop/recovery failed: {recovery_error}");
    Some(match error {
        Some(error) => format!("{error}; {teardown}"),
        None => teardown,
    })
}

pub trait Cleanup {
    fn stop(&mut self) -> bool;
    fn restore_volume(&mut self) -> Option<String>;
    fn restore_source(&mut self) -> Option<String>;
    fn report(&mut self, volume: Option<String>, source: Option<String>);
}

pub struct CleanupGuard<T: Cleanup> {
    pub context: T,
    finished: CleanupToken,
}

impl<T: Cleanup> CleanupGuard<T> {
    pub fn new(context: T) -> Self {
        Self {
            context,
            finished: CleanupToken(Rc::new(Cell::new(false))),
        }
    }

    pub fn token(&self) -> CleanupToken {
        self.finished.clone()
    }

    pub fn finish(&mut self) {
        if self.finished.0.replace(true) {
            return;
        }
        let stopped = self.context.stop();
        let volume = if stopped {
            self.context.restore_volume()
        } else {
            Some("skipped: stream may still be running".into())
        };
        let source = self.context.restore_source();
        self.context.report(volume, source);
    }
}

impl<T: Cleanup> Drop for CleanupGuard<T> {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Actions {
        events: Rc<RefCell<Vec<String>>>,
        stopped: bool,
    }
    impl Cleanup for Actions {
        fn stop(&mut self) -> bool {
            self.events.borrow_mut().push("stop".into());
            self.stopped
        }
        fn restore_volume(&mut self) -> Option<String> {
            self.events.borrow_mut().push("volume".into());
            Some("volume failure".into())
        }
        fn restore_source(&mut self) -> Option<String> {
            self.events.borrow_mut().push("source".into());
            Some("source failure".into())
        }
        fn report(&mut self, volume: Option<String>, source: Option<String>) {
            self.events
                .borrow_mut()
                .push(format!("{volume:?};{source:?}"));
        }
    }

    #[test]
    fn failed_teardown_fails_run_even_after_successful_pcm_and_preserves_stream_error() {
        assert_eq!(
            teardown_error(None, "close timed out".into()).as_deref(),
            Some("USB audio stop/recovery failed: close timed out")
        );
        assert_eq!(
            teardown_error(Some("write failed".into()), "close timed out".into()).as_deref(),
            Some("write failed; USB audio stop/recovery failed: close timed out")
        );
    }

    #[test]
    fn teardown_failure_skips_volume_but_reports_both_restores_once() {
        let events = Rc::new(RefCell::new(Vec::new()));
        {
            let mut guard = CleanupGuard::new(Actions {
                events: events.clone(),
                stopped: false,
            });
            guard.finish();
            guard.finish();
        }
        assert_eq!(
            &*events.borrow(),
            &[
                "stop",
                "source",
                "Some(\"skipped: stream may still be running\");Some(\"source failure\")"
            ]
        );
    }

    #[test]
    fn panic_hook_cleans_before_unwinding_and_drop_does_not_repeat() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = CleanupGuard::new(Actions {
                events: events.clone(),
                stopped: true,
            });
            let _panic = guard.token().arm_on_panic(Actions {
                events: events.clone(),
                stopped: true,
            });
            // Observe cleanup during the hook, before any stack destructor.
            struct Observe(Rc<RefCell<Vec<String>>>);
            impl Drop for Observe {
                fn drop(&mut self) {
                    assert_eq!(self.0.borrow().len(), 4);
                }
            }
            let _observe = Observe(observed);
            panic!("after session volume, before stream teardown");
        }));
        assert!(result.is_err());
        assert_eq!(events.borrow().len(), 4);
        assert_eq!(events.borrow()[0], "stop");
        assert_eq!(events.borrow()[1], "volume");
    }

    #[test]
    fn normal_exit_disarms_panic_hook_and_cleanup_runs_once() {
        let events = Rc::new(RefCell::new(Vec::new()));
        {
            let mut guard = CleanupGuard::new(Actions {
                events: events.clone(),
                stopped: true,
            });
            let registration = guard.token().arm_on_panic(Actions {
                events: events.clone(),
                stopped: true,
            });
            drop(registration);
            guard.finish();
        }
        assert_eq!(events.borrow().len(), 4);
        assert!(PANIC_CLEANUP.with(|slot| slot.borrow().is_none()));
    }

    #[test]
    fn unwinding_stops_before_restoring_and_reports_once() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let events = events.clone();
            move || {
                let _guard = CleanupGuard::new(Actions {
                    events,
                    stopped: true,
                });
                panic!("after setting session volume");
            }
        }));
        assert!(result.is_err());
        assert_eq!(
            &*events.borrow(),
            &[
                "stop",
                "volume",
                "source",
                "Some(\"volume failure\");Some(\"source failure\")"
            ]
        );
    }
}
