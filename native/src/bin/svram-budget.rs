//! Bounded hardware probe, separate from the minimal WinPE storage test CLI.
use std::time::{Duration, Instant};
use swappable_vram_engine::store::{MIB, VramStore};
use swappable_vram_engine::wddm::{BudgetMonitor, BudgetSample, MemoryFootprint, memory_footprint};
fn json(value: BudgetSample) -> String {
    format!(
        "{{\"localBudget\":{},\"localUsage\":{},\"localReservation\":{},\"reservationAllowance\":{},\"nonLocalBudget\":{},\"nonLocalUsage\":{}}}",
        value.local.budget,
        value.local.usage,
        value.local.reservation,
        value.local.available_for_reservation,
        value.non_local.budget,
        value.non_local.usage
    )
}
fn ram_json(value: MemoryFootprint) -> String {
    format!(
        "{{\"systemAvailablePhysical\":{},\"systemAvailableCommit\":{},\"processWorkingSet\":{},\"processPrivateCommit\":{}}}",
        value.available_physical,
        value.available_commit,
        value.process_working_set,
        value.process_private_commit
    )
}
fn run() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .ok_or("svram-budget <new-report.json> [16|64|256|512 MiB]")?;
    let mib = std::env::args()
        .nth(2)
        .map(|s| s.parse::<u64>().map_err(|e| e.to_string()))
        .transpose()?
        .unwrap_or(64);
    if !matches!(mib, 16 | 64 | 256 | 512) {
        return Err("Probe size must be 16, 64, 256 or 512 MiB".into());
    }
    let monitor = BudgetMonitor::for_cuda(0)?;
    let before = monitor.sample()?;
    let ram_before = memory_footprint()?;
    let bytes = mib * MIB;
    if before.local.allowance() < bytes + 256 * MIB {
        return Err("Insufficient process-local budget for the bounded probe".into());
    }
    let store = VramStore::new(0, bytes)?; // Also protects global game headroom.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut allocated = monitor.sample()?;
    while allocated.local.usage.saturating_sub(before.local.usage) < bytes
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
        allocated = monitor.sample()?;
    }
    let correlated = allocated.local.usage.saturating_sub(before.local.usage) >= bytes;
    let ram_allocated = memory_footprint()?;
    // Empty event waits must time out; a signalled budget change is not itself
    // proof of this CUDA allocation, so both raw observations remain in report.
    let notification = monitor.wait_changed(0)?;
    drop(store);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut released = monitor.sample()?;
    while released.local.usage > before.local.usage + 4 * MIB && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
        released = monitor.sample()?;
    }
    let ram_released = memory_footprint()?;
    let release_reflected = released.local.usage <= before.local.usage + 4 * MIB;
    let report = format!(
        "{{\"schema\":2,\"cudaOrdinal\":0,\"luid\":{},\"node\":{},\"testBytes\":{},\"cudaUsageCorrelated\":{},\"releaseReflected\":{},\"budgetEventObserved\":{},\"before\":{},\"allocated\":{},\"released\":{},\"ramBefore\":{},\"ramAllocated\":{},\"ramReleased\":{},\"pagingBackendQualified\":false,\"gameBenefitProven\":false}}\n",
        monitor.luid,
        monitor.node,
        bytes,
        correlated,
        release_reflected,
        notification,
        json(before),
        json(allocated),
        json(released),
        ram_json(ram_before),
        ram_json(ram_allocated),
        ram_json(ram_released)
    );
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?
        .write_all(report.as_bytes())
        .map_err(|e| e.to_string())?;
    println!("{report}");
    if !correlated || !release_reflected {
        return Err("CUDA allocation was not reflected in the matched WDDM local usage; admission must remain disabled".into());
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
