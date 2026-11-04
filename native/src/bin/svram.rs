//! Small native-only diagnostic runner; no changes to Windows paging settings.
use swappable_vram_engine::{
    cache::{CachedOrigin, PAGE},
    cuda,
    lab_origin::LabOrigin,
    store::{MIB, VramStore},
};
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("broker-host") => {
            let pipe=args.get(2).ok_or("broker-host <pipe-name> <parent-pid> <nonce> [--experimental]")?;
            let parent=args.get(3).ok_or("Missing GUI PID")?.parse::<u32>().map_err(|e|e.to_string())?;
            let nonce=args.get(4).ok_or("Missing control nonce")?;
            if args.len()>6 || args.get(5).is_some_and(|arg|arg!="--experimental") { return Err("Invalid broker-host option".into()); }
            swappable_vram_engine::broker_host::run(pipe,parent,nonce,args.len()==6)?;
        }
        Some("io-smoke") => {
            swappable_vram_engine::overlapped_smoke::run()?;
            swappable_vram_engine::driver_broker::local_control_smoke()?;
        }
        Some("range-benchmark") => {
            let output=args.get(2).ok_or("range-benchmark <new-report.json>")?;
            swappable_vram_engine::range_benchmark::run(std::path::Path::new(output))?;
        }
        Some("host-status") => {
            let options=swappable_vram_engine::guest_lab::code_integrity_options()?;
            println!("Runtime code integrity options: {options:#010x}; test-signing={}",options & 2 != 0);
            let interfaces=swappable_vram_engine::device_interfaces::filters()?;
            println!("Local SwappableVRAM driver interfaces: {}",interfaces.len());
            for ordinal in 0..cuda::device_count()? {
                let info=cuda::device_info(ordinal)?;
                println!("Local GPU {ordinal}: {} / {} MiB",String::from_utf8_lossy(&info.name).trim_end_matches('\0'), info.total_bytes/MIB);
            }
            println!("Read-only inspection; no driver, pagefile, working-set or boot changes");
        }
        Some("lab-enable-io-verifier") => {
            swappable_vram_engine::guest_lab::enable_io_verifier()?;
        }
        Some("lab-cache-scenario") => {
            let boot=args.get(2).ok_or("lab-cache-scenario <owned-boot-root> <miss|late|exit>")?;
            let behavior=args.get(3).ok_or("Missing kernel fault behavior")?;
            swappable_vram_engine::guest_cache::run(std::path::Path::new(boot),behavior)?;
        }
        Some("lab-prepare-storage") => {
            swappable_vram_engine::guest_disk::prepare()?;
        }
        Some("lab-code-integrity") => {
            swappable_vram_engine::guest_lab::require_test_signing()?;
        }
        Some("lab-check-bypass") => {
            swappable_vram_engine::guest_lab::check_bypass()?;
        }
        Some("lab-environment") => {
            let environment = if args.get(2).map(String::as_str) == Some("--require-guest") {
                swappable_vram_engine::guest_lab::require_guest()?
            } else {
                swappable_vram_engine::guest_lab::environment()?
            };
            println!("{environment:?}");
        }
        Some("lab-trust") => {
            let path = args.get(2).ok_or("lab-trust <disposable-guest-certificate.cer>")?;
            swappable_vram_engine::guest_lab::trust_disposable_certificate(std::path::Path::new(path))?;
        }
        Some("driver-fixture") => {
            if args.get(5).map(String::as_str) != Some("--isolated-test") {
                return Err("driver-fixture <private-interface> <seconds> <cache|miss|late|exit> --isolated-test; GPU-free kernel fixture only".into());
            }
            let path = args.get(2).ok_or("Missing private interface")?;
            let seconds = args.get(3).ok_or("Missing duration")?.parse::<u64>().map_err(|e| e.to_string())?;
            let behavior = args.get(4).ok_or("Missing fixture behavior")?;
            swappable_vram_engine::driver_broker::run_fixture(path, seconds, behavior, args.get(6).map(String::as_str)==Some("--stop-on-stdin"))?;
        }
        Some("driver-list") => {
            let interfaces = swappable_vram_engine::device_interfaces::filters()?;
            println!("Present private filter interfaces: {}", interfaces.len());
            for interface in interfaces {
                println!("Instance: {}\nInterface: {}", interface.instance_id, interface.path);
            }
        }
        Some("driver-query") => {
            let path = args.get(2).ok_or("driver-query <private-filter-interface>")?;
            println!("{:?}", swappable_vram_engine::driver_broker::query(path)?);
        }
        Some("driver-broker") => {
            let path = args.get(2).ok_or("driver-broker <private-filter-interface> <seconds>")?;
            let seconds = args.get(3).ok_or("Explicit bounded duration is required")?.parse::<u64>().map_err(|e| e.to_string())?;
            swappable_vram_engine::driver_broker::run(path, seconds)?;
        }
        Some("lab-broker") => {
            let session = args.get(2).ok_or("lab-broker <private-session>")?;
            swappable_vram_engine::transport_lab::serve(session, 0)?;
        }
        Some("ipc-benchmark") => {
            let output = args.get(2).ok_or("ipc-benchmark <report.json>")?;
            swappable_vram_engine::ipc_benchmark::run(std::path::Path::new(output))?;
        }
        Some("cache-smoke") => {
            let dir = args.get(2).ok_or("cache-smoke <existing-test-directory>")?;
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos();
            let path = std::path::Path::new(dir)
                .join(format!("svram-{}-{unique}.svram-lab", std::process::id()));
            let origin = LabOrigin::create_new(&path, 32 * MIB)?;
            let vram = VramStore::new(0, 16 * MIB)?;
            let mut tier = CachedOrigin::new(origin, vram)?;
            // 8 MiB of real pages. Test hit, durable origin, and revocation.
            for index in 0..2048 {
                tier.write_page(index, &[(index % 251) as u8; PAGE])?;
            }
            for index in 0..2048 {
                let mut read = [0; PAGE];
                tier.read_page(index, &mut read)?;
                if read != [(index % 251) as u8; PAGE] {
                    return Err(format!("Cache integrity failure at {index}"));
                }
            }
            if tier.stats.hits != 2048 {
                return Err("Expected 2048 GPU cache hits".into());
            }
            tier.release_cache();
            for index in 0..2048 {
                let mut read = [0; PAGE];
                tier.read_page(index, &mut read)?;
                if read != [(index % 251) as u8; PAGE] {
                    return Err(format!(
                        "Origin integrity failure after GPU release at {index}"
                    ));
                }
            }
            println!(
                "PASS hardware cache / direct origin / GPU release: {:?}",
                tier.stats
            );
            drop(tier);
            if path.exists() {
                return Err("Lab origin cleanup failed".into());
            }
        }
        Some("benchmark") => {
            let store = VramStore::new(0, 64 * MIB)?;
            let mut write = vec![0.0; 2000];
            let mut read = vec![0.0; 2000];
            store.measure(true, 0, &mut write)?;
            store.measure(false, 0, &mut read)?;
            for (name, mut values) in [("write", write), ("read", read)] {
                values.sort_by(f64::total_cmp);
                println!(
                    "{name} QD1 4KiB 2000 samples: p50={:.1} p95={:.1} p99={:.1} max={:.1} us",
                    values[999], values[1899], values[1979], values[1999]
                );
            }
        }
        None | Some("devices") => {
            for ordinal in 0..cuda::device_count()? {
                let info = cuda::device_info(ordinal)?;
                let name = String::from_utf8_lossy(&info.name);
                println!(
                    "GPU {}: {} / {} MiB",
                    ordinal,
                    name.trim_end_matches('\0'),
                    info.total_bytes / MIB
                );
            }
        }
        _ => return Err("devices | benchmark | cache-smoke <existing-test-directory> | ipc-benchmark <report.json> | driver-list | driver-query <private-interface> | driver-broker <private-interface> <seconds>".into()),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
