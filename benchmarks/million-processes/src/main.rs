//! Benchmark: spawn N lightweight BEAM processes on a single beamr instance.
//!
//! Each process sits in a `receive` loop — simulating a sleeping/waiting
//! durable workflow. This demonstrates the per-process memory overhead that
//! makes Aion competitive at scale where goroutine-based engines cannot.
//!
//! Usage: `cargo run --release -p million-processes [count]`
//!   count defaults to `100_000`

use std::time::Instant;

use aion::{RuntimeConfig, RuntimeHandle, RuntimeInput};

const FIXTURE_BEAM: &[u8] =
    include_bytes!("../../../crates/aion/tests/fixtures/aion_fixture_workflow.beam");
const MODULE_NAME: &str = "aion_fixture_workflow";
const ENTRY_FUNCTION: &str = "wait";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.replace('_', "").parse().ok())
        .unwrap_or(100_000);

    println!("=== Aion / beamr Process Benchmark ===\n");
    println!("Target: {count} sleeping processes on a single node\n");

    let mem_before = resident_memory_bytes();

    let t_boot = Instant::now();
    let runtime = RuntimeHandle::new(RuntimeConfig::new(None))?;
    runtime.register_module(MODULE_NAME, FIXTURE_BEAM)?;
    let boot_ms = t_boot.elapsed().as_millis();
    println!("Runtime boot:       {boot_ms}ms");

    let mut pids = Vec::with_capacity(count);
    let t_spawn = Instant::now();
    for _ in 0..count {
        let pid = runtime.spawn_workflow(MODULE_NAME, ENTRY_FUNCTION, RuntimeInput::default())?;
        pids.push(pid);
    }
    let spawn_elapsed = t_spawn.elapsed();
    let spawn_ms = spawn_elapsed.as_millis();
    let per_spawn_ns = spawn_elapsed.as_nanos() / count as u128;
    println!("Spawned {count} processes: {spawn_ms}ms ({per_spawn_ns}ns/process)");

    let mem_after = resident_memory_bytes();
    let mem_delta = mem_after.saturating_sub(mem_before);
    let per_process = mem_delta.checked_div(count).unwrap_or(0);

    println!("\nMemory:");
    println!("  Before:           {}", human_bytes(mem_before));
    println!("  After:            {}", human_bytes(mem_after));
    println!("  Delta:            {}", human_bytes(mem_delta));
    println!("  Per process:      ~{per_process} bytes");

    println!("\n--- Comparison ---");
    println!("  Temporal (Go goroutine): ~8,192 bytes/workflow minimum");
    println!("  Aion (beamr process):    ~{per_process} bytes/workflow");
    if per_process > 0 {
        let ratio = 8192 / per_process.max(1);
        println!("  Density advantage:       ~{ratio}x more workflows per node");
    }

    println!("\nAll {count} processes alive and waiting.");
    println!("Press Ctrl+C to exit, or waiting 3 seconds...\n");
    std::thread::sleep(std::time::Duration::from_secs(3));

    let t_shutdown = Instant::now();
    runtime.shutdown()?;
    let shutdown_ms = t_shutdown.elapsed().as_millis();
    println!("Shutdown:           {shutdown_ms}ms");
    println!("\nDone.");

    Ok(())
}

fn resident_memory_bytes() -> usize {
    // macOS: use mach task_info
    #[cfg(target_os = "macos")]
    {
        macos_rss().unwrap_or(0)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux fallback: read /proc/self/statm
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
            .map_or(0, |pages| pages * 4096)
    }
}

// Reading resident-set size on macOS requires the `task_info` mach syscall,
// which has no safe wrapper in std; the FFI block and calls are unavoidable.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn macos_rss() -> Option<usize> {
    use std::mem;

    #[repr(C)]
    struct TaskBasicInfo {
        suspend_count: i32,
        virtual_size: u64,
        resident_size: u64,
        user_time: [u32; 2],
        system_time: [u32; 2],
        policy: i32,
    }

    const MACH_TASK_BASIC_INFO: u32 = 20;
    // Word count of a small fixed struct — provably tiny, so the narrowing cast
    // cannot truncate; expressed as a const to keep it compile-time.
    #[allow(clippy::cast_possible_truncation)]
    const MACH_TASK_BASIC_INFO_COUNT: u32 =
        (mem::size_of::<TaskBasicInfo>() / mem::size_of::<u32>()) as u32;

    unsafe extern "C" {
        fn mach_task_self() -> u32;
        fn task_info(target: u32, flavor: u32, info: *mut TaskBasicInfo, count: *mut u32) -> i32;
    }

    let mut info: TaskBasicInfo = unsafe { mem::zeroed() };
    let mut count = MACH_TASK_BASIC_INFO_COUNT;
    let result = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &raw mut info,
            &raw mut count,
        )
    };
    if result == 0 {
        usize::try_from(info.resident_size).ok()
    } else {
        None
    }
}

// Casts are display-only approximations for human-readable byte sizes; the
// precision loss at large magnitudes is intentional and harmless.
#[allow(clippy::cast_precision_loss)]
fn human_bytes(bytes: usize) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
