//! Pure admission policy. It does not allocate VRAM, trim processes, or change
//! OS settings. A paging driver will need separate qualification before use.
use crate::store::MIB;

#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub gpu_total: u64,
    pub gpu_free: u64,
    /// WDDM process-local allowance is separate from global free VRAM. Unknown
    /// budget refuses admission rather than treating global free as a budget.
    pub wddm_allowance: Option<u64>,
    /// WDDM backing can consume commit without occupying equal physical RAM.
    pub available_commit: Option<u64>,
    pub available_ram: u64,
    pub total_ram: u64,
    pub recent_gpu_p99_us: Option<f64>,
    pub qualified_paging_backend: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Disabled(&'static str),
    Eligible { maximum_bytes: u64 },
}
pub fn decide(o: Observation, configured_cap: u64, read_deadline_us: f64) -> Decision {
    if !o.qualified_paging_backend {
        return Decision::Disabled("paging backend is not qualified");
    }
    if o.gpu_total == 0
        || o.gpu_free > o.gpu_total
        || o.total_ram == 0
        || o.available_ram > o.total_ram
    {
        return Decision::Disabled("invalid memory telemetry");
    }
    let Some(budget) = o.wddm_allowance else {
        return Decision::Disabled("WDDM allowance is unknown");
    };
    let Some(commit) = o.available_commit else {
        return Decision::Disabled("system commit allowance is unknown");
    };
    if commit < 576 * MIB {
        return Decision::Disabled("insufficient commit headroom");
    }
    let Some(latency) = o.recent_gpu_p99_us else {
        return Decision::Disabled("latency evidence is missing");
    };
    if !latency.is_finite()
        || latency <= 0.0
        || !read_deadline_us.is_finite()
        || read_deadline_us <= 0.0
        || latency >= read_deadline_us
    {
        return Decision::Disabled("cache cannot meet read deadline");
    }
    if o.available_ram >= (2 * 1024 * MIB).max(o.total_ram / 5) {
        return Decision::Disabled("no measured RAM pressure");
    }
    let reserve = (2 * 1024 * MIB).max(o.gpu_total / 5);
    let maximum = configured_cap
        .min(budget)
        .min(commit.saturating_sub(512 * MIB))
        .min(o.gpu_free.saturating_sub(reserve));
    let aligned = maximum / (64 * MIB) * (64 * MIB);
    if aligned < 64 * MIB {
        Decision::Disabled("insufficient GPU headroom")
    } else {
        Decision::Eligible {
            maximum_bytes: aligned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid() -> Observation {
        Observation {
            gpu_total: 16 * 1024 * MIB,
            gpu_free: 10 * 1024 * MIB,
            wddm_allowance: Some(5 * 1024 * MIB),
            available_commit: Some(16 * 1024 * MIB),
            available_ram: 512 * MIB,
            total_ram: 8 * 1024 * MIB,
            recent_gpu_p99_us: Some(50.0),
            qualified_paging_backend: true,
        }
    }
    #[test]
    fn commit_backing_cannot_overrun_remaining_system_commit() {
        let mut observation = valid();
        observation.available_commit = None;
        assert!(matches!(
            decide(observation, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
        observation.available_commit = Some(512 * MIB);
        assert!(matches!(
            decide(observation, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
        observation.available_commit = Some(768 * MIB);
        assert_eq!(
            decide(observation, 1024 * MIB, 100.0),
            Decision::Eligible {
                maximum_bytes: 256 * MIB
            }
        );
    }
    #[test]
    fn admission_bounds_capacity_by_all_limits() {
        assert_eq!(
            decide(valid(), 8 * 1024 * MIB, 100.0),
            Decision::Eligible {
                maximum_bytes: 5 * 1024 * MIB
            }
        );
        let mut o = valid();
        o.gpu_free = 4 * 1024 * MIB;
        assert_eq!(
            decide(o, 8 * 1024 * MIB, 100.0),
            Decision::Eligible {
                maximum_bytes: 768 * MIB
            }
        );
    }
    #[test]
    fn unknown_budget_never_uses_global_free_as_substitute() {
        let mut o = valid();
        o.wddm_allowance = None;
        assert!(matches!(
            decide(o, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
    }
    #[test]
    fn unqualified_driver_never_admits() {
        let mut o = valid();
        o.qualified_paging_backend = false;
        assert!(matches!(
            decide(o, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
    }
    #[test]
    fn slower_tail_or_missing_pressure_disables_cache() {
        let mut o = valid();
        o.recent_gpu_p99_us = Some(100.0);
        assert!(matches!(
            decide(o, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
        o = valid();
        o.available_ram = 4 * 1024 * MIB;
        assert!(matches!(
            decide(o, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
        o = valid();
        o.recent_gpu_p99_us = Some(f64::NAN);
        assert!(matches!(
            decide(o, 1024 * MIB, 100.0),
            Decision::Disabled(_)
        ));
    }
}
