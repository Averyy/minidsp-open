//! Isolated allocator instrumentation for the rattle plan and renderer.

use minidsp_protocol::rattle::{RattlePlan, RattleRequest};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;

thread_local! {
    // Count only this test thread while a measured operation is running.
    static ALLOCATIONS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

fn record_allocation(bytes: usize) {
    let _ = ALLOCATIONS.try_with(|counter| {
        if let Some((count, total)) = counter.get() {
            counter.set(Some((count + 1, total + bytes)));
        }
    });
}

// SAFETY: Every operation delegates to System with the original pointer/layout.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measured<T>(operation: impl FnOnce() -> T) -> (T, (usize, usize)) {
    ALLOCATIONS.with(|counter| counter.set(Some((0, 0))));
    let result = operation();
    let allocations = ALLOCATIONS.with(|counter| counter.take().unwrap());
    (result, allocations)
}

#[test]
fn plan_and_generator_allocations_do_not_grow_with_steps() {
    let mut measurements = Vec::new();
    for cycles in [11, 4096] {
        // Equal tone/volume dimensions isolate growth in the step count.
        // A low valid sample rate lets us render the entire maximum session.
        let request = RattleRequest {
            frequencies_hz: Some(vec![5.0]),
            volumes_db: vec![-35.0],
            tone_sec: 0.5,
            gap_sec: 0.0,
            cycles,
            sample_rate: 11,
            ..RattleRequest::default()
        };
        let (plan, build) = measured(|| RattlePlan::build(request).unwrap());
        assert_eq!(plan.step_count(), cycles as usize);
        let (_, clone_plan) = measured(|| plan.clone());
        let (mut generator, create_generator) = measured(|| plan.generator());
        let (_, clone_generator) = measured(|| generator.clone());
        let (_, retry_generator) = measured(|| plan.generator());
        let (_, render) = measured(|| {
            let mut buffer = [0; 997];
            while !generator.is_finished() {
                assert!(generator.fill_pcm_bytes(&mut buffer) > 0);
            }
        });
        assert_eq!(render, (0, 0), "rendering must never allocate labels");
        assert_eq!(generator.cursor(), plan.total_samples());
        measurements.push((
            build,
            clone_plan,
            create_generator,
            clone_generator,
            retry_generator,
        ));
    }
    assert_eq!(measurements[0], measurements[1]);
}
