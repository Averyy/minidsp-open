//! Route one task's heap allocations to PSRAM for the length of a guard.
//!
//! ESP-IDF's malloc keeps allocations under CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL
//! (16 KiB) in internal RAM, which USB host transfers, WiFi and task stacks
//! also need. A device snapshot is about a thousand small allocations, roughly
//! 40 KB per copy; a restore holds the request's copy and builds a verification
//! copy, which left 16 bytes of internal RAM and stalled USB until the board
//! hung. Handlers that build snapshots run under [`PreferPsram`].

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};
use esp_idf_svc::sys;
use std::alloc::System;

/// The task whose allocations go to PSRAM; null when none.
static PSRAM_TASK: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(ptr::null_mut());

pub struct Allocator;

fn prefer_psram() -> bool {
    let task = PSRAM_TASK.load(Ordering::Relaxed);
    !task.is_null() && task == unsafe { sys::xTaskGetCurrentTaskHandle() }.cast()
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

/// While alive, the current task's allocations prefer PSRAM, falling back to
/// internal RAM when PSRAM is full. One task at a time; a second guard on
/// another task leaves that task's allocations where they were.
pub struct PreferPsram {
    claimed: bool,
}

impl PreferPsram {
    pub fn enter() -> Self {
        let task = unsafe { sys::xTaskGetCurrentTaskHandle() }.cast();
        let claimed = PSRAM_TASK
            .compare_exchange(ptr::null_mut(), task, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok();
        PreferPsram { claimed }
    }
}

impl Drop for PreferPsram {
    fn drop(&mut self) {
        if self.claimed {
            PSRAM_TASK.store(ptr::null_mut(), Ordering::Release);
        }
    }
}
