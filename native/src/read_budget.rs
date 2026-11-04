//! Broker-side work avoidance. The kernel's independent timer and final clock
//! check remain authoritative; this cannot promise an OS completion deadline.
pub(crate) const GPU_HEADROOM_TICKS: u64 = 2500; // 250 us in the 100 ns interrupt-time domain.
pub(crate) fn now() -> u64 {
    let mut ticks = 0;
    unsafe {
        QueryInterruptTimePrecise(&mut ticks);
    }
    ticks
}
pub(crate) fn can_lookup(deadline: u64, now: u64) -> bool {
    deadline
        .checked_sub(now)
        .is_some_and(|remaining| remaining >= GPU_HEADROOM_TICKS)
}
pub(crate) fn can_reply_hit(deadline: u64, now: u64) -> bool {
    now < deadline
}
#[link(name = "mincore")]
unsafe extern "system" {
    fn QueryInterruptTimePrecise(ticks: *mut u64);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_interrupt_time_api_is_available_and_monotonic() {
        let first = now();
        let second = now();
        assert!(first > 0 && second >= first);
    }
    #[test]
    fn expired_and_nearly_expired_work_is_never_admitted() {
        assert!(!can_lookup(10000, 10000));
        assert!(!can_lookup(10000, 10001));
        assert!(!can_lookup(10000, 7501));
        assert!(can_lookup(10000, 7500));
        assert!(!can_reply_hit(10000, 10000));
        assert!(can_reply_hit(10000, 9999));
        assert!(!can_lookup(0, u64::MAX));
    }
}
