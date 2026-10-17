//! Run only inside a ready Slurm allocation. Build with the installed local profile.
use std::alloc::GlobalAlloc;
use std::alloc::Layout;
use std::alloc::System;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant;

struct Counted;
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
// SAFETY: delegation preserves System's allocation/deallocation contracts.
// The counter measures allocations, not timing-path synchronization overhead.
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn main() -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::var_os("SLURM_JOB_ID").is_some(),
        "measurements require Slurm admission"
    );
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let active = match args.first().map(String::as_str) {
        Some("baseline") => false,
        Some("active") => true,
        _ => anyhow::bail!("usage: ui_latency_overhead baseline|active [iterations]"),
    };
    let iterations = args.get(1).map_or(Ok(1_000_000_u64), |arg| arg.parse())?;
    anyhow::ensure!(
        (1..=100_000_000).contains(&iterations),
        "iterations outside fixed harness bound"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(codex_tui::ui_latency::benchmark(active, |boundary, recorder_bytes| {
        for nested in [false, true] {
            for _ in 0..10_000 { boundary(nested); }
            for sample in 0..7 {
                let allocations = ALLOCATIONS.load(Ordering::Relaxed);
                let started = Instant::now();
                for _ in 0..iterations { boundary(nested); }
                let elapsed = started.elapsed().as_nanos();
                let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
                let boundaries = iterations * if nested { 2 } else { 1 };
                println!("{{\"arm\":\"{}\",\"nested\":{nested},\"sample\":{sample},\"iterations\":{iterations},\"boundaries\":{boundaries},\"elapsed_ns\":{elapsed},\"ns_per_boundary\":{},\"allocations\":{allocations},\"recorder_bytes\":{recorder_bytes}}}",
                    if active { "active" } else { "baseline" }, elapsed as f64 / boundaries as f64);
            }
        }
    }));
    Ok(())
}
