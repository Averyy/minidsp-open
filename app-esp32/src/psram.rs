//! Send chosen tasks' heap allocations to PSRAM.
//!
//! ESP-IDF's malloc keeps allocations under CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL
//! (16 KiB) in internal RAM, which USB host transfers, WiFi and task stacks
//! also need. HTTP handlers build large structures out of small allocations:
//! a snapshot restore (about a thousand, held twice) left 16 bytes and hung
//! USB, and measurement mode starved the WiFi PHY into an abort. The HTTP
//! server task is registered at startup, so every handler allocates in PSRAM,
//! and the playback worker registers itself for its per-write statistics.
//! The main task builds each connected device's state inside [`external`].
//! Buffers handed to the flash driver use [`internal`]: it writes a PSRAM
//! source 32 bytes at a time (reads go through its internal bounce buffer).

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};
use esp_idf_svc::sys;
use std::alloc::System;

/// Tasks whose allocations go to PSRAM; null slots are free.
static PSRAM_TASKS: [AtomicPtr<c_void>; 4] = [const { AtomicPtr::new(ptr::null_mut()) }; 4];
/// A registered task inside [`internal`], whose allocations stay internal.
static INTERNAL_TASK: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

pub struct Allocator;

fn current_task() -> *mut c_void {
    unsafe { sys::xTaskGetCurrentTaskHandle() }.cast()
}

fn prefer_psram() -> bool {
    let task = current_task();
    !task.is_null()
        && PSRAM_TASKS
            .iter()
            .any(|slot| slot.load(Ordering::Relaxed) == task)
        && INTERNAL_TASK.load(Ordering::Relaxed) != task
}

unsafe fn psram_alloc(layout: Layout) -> *mut u8 {
    unsafe {
        sys::heap_caps_aligned_alloc(
            layout.align().max(4),
            layout.size(),
            sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT,
        )
    }
    .cast()
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if prefer_psram() {
            let block = unsafe { psram_alloc(layout) };
            if !block.is_null() {
                return block;
            }
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        // free() returns a block to whichever heap holds it.
        unsafe { System.dealloc(block, layout) }
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if !prefer_psram() {
            return unsafe { System.realloc(block, layout, new_size) };
        }
        // realloc() would move a small block back to internal RAM.
        let new_layout = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        let moved = unsafe { self.alloc(new_layout) };
        if !moved.is_null() {
            unsafe {
                ptr::copy_nonoverlapping(block, moved, layout.size().min(new_size));
                self.dealloc(block, layout);
            }
        }
        moved
    }
}

/// From now on, the named task's allocations prefer PSRAM, falling back to
/// internal RAM when PSRAM is full. False if the task doesn't exist or every
/// slot is taken.
pub fn prefer_for_task(name: &core::ffi::CStr) -> bool {
    let task: *mut c_void = unsafe { sys::xTaskGetHandle(name.as_ptr()) }.cast();
    if task.is_null() {
        return false;
    }
    PSRAM_TASKS.iter().any(|slot| {
        slot.load(Ordering::Acquire) == task
            || slot
                .compare_exchange(ptr::null_mut(), task, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    })
}

/// Run `f` with the current task's allocations in internal RAM, for buffers
/// the flash driver reads. One task at a time; a second caller runs `f`
/// unchanged.
pub fn internal<T>(f: impl FnOnce() -> T) -> T {
    struct Release(bool);
    impl Drop for Release {
        fn drop(&mut self) {
            if self.0 {
                INTERNAL_TASK.store(ptr::null_mut(), Ordering::Release);
            }
        }
    }
    let _release = Release(
        INTERNAL_TASK
            .compare_exchange(
                ptr::null_mut(),
                current_task(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok(),
    );
    f()
}

/// Run `f` with the current task's allocations in PSRAM, for long-lived state
/// built by a task that isn't registered. Takes a free slot for the duration;
/// with none free, `f` runs unchanged.
pub fn external<T>(f: impl FnOnce() -> T) -> T {
    struct Release(Option<&'static AtomicPtr<c_void>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(slot) = self.0 {
                slot.store(ptr::null_mut(), Ordering::Release);
            }
        }
    }
    let task = current_task();
    let registered = PSRAM_TASKS
        .iter()
        .any(|slot| slot.load(Ordering::Acquire) == task);
    let _release = Release(if registered || task.is_null() {
        None
    } else {
        PSRAM_TASKS.iter().find(|slot| {
            slot.compare_exchange(ptr::null_mut(), task, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    });
    f()
}
