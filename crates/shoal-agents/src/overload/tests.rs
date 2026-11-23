use super::*;

const CALM: MemoryPressure = MemoryPressure {
    critical: false,
    stalled: false,
    warning: false,
    safe: true,
};

/// Past the warning level of memory in use.
const WARNING: MemoryPressure = MemoryPressure {
    warning: true,
    safe: false,
    ..CALM
};

/// Past the stop level of memory in use.
const CRITICAL: MemoryPressure = MemoryPressure {
    critical: true,
    ..WARNING
};

/// Every task stalls on memory past the stop level.
const STALLED: MemoryPressure = MemoryPressure {
    stalled: true,
    ..WARNING
};

#[test]
fn memory_in_use_stops_at_once_and_stall_once_it_holds() {
    let start = Instant::now();
    let settings = crate::config::overload::Overload::default();
    let ticks = || Ok(crate::process::load::CpuTicks::default());
    let poll = Duration::from_secs(settings.poll_seconds);
    let sustained = Duration::from_secs(settings.memory.sustained_seconds);
    let mut monitor = Monitor::default();
    assert_eq!(
        monitor.reason(start, &settings, Some(CRITICAL), ticks()),
        Some(Pressure::Memory)
    );
    let mut monitor = Monitor::default();
    let mut at = start;
    while at < start + sustained {
        assert_eq!(monitor.reason(at, &settings, Some(STALLED), ticks()), None);
        at += poll;
    }
    assert_eq!(
        monitor.reason(at, &settings, Some(STALLED), ticks()),
        Some(Pressure::Memory)
    );
    // A failed reading restarts the stall's window.
    assert_eq!(monitor.reason(at + poll, &settings, None, ticks()), None);
    assert!(monitor.memory_stall.since.is_none());
    assert_eq!(
        monitor.reason(at + poll * 2, &settings, Some(STALLED), ticks()),
        None
    );
}

#[test]
fn cpu_is_opt_in_and_requires_continuous_samples() {
    let start = Instant::now();
    let mut settings = crate::config::overload::Overload {
        poll_seconds: 100,
        ..Default::default()
    };
    let mut monitor = Monitor::default();
    let ticks = |n: u64| {
        Ok(crate::process::load::CpuTicks {
            used: 95 * n,
            niced: 0,
            total: 100 * n,
        })
    };
    assert_eq!(monitor.reason(start, &settings, Some(CALM), ticks(0)), None);
    assert_eq!(
        monitor.reason(
            start + Duration::from_secs(600),
            &settings,
            Some(CALM),
            ticks(1)
        ),
        None
    );
    settings.cpu.enabled = true;
    assert_eq!(
        monitor.reason(
            start + Duration::from_secs(601),
            &settings,
            Some(CALM),
            ticks(2)
        ),
        None
    );
    assert_eq!(
        monitor.reason(
            start + Duration::from_secs(900),
            &settings,
            Some(CALM),
            ticks(3)
        ),
        None
    );
    assert_eq!(
        monitor.reason(
            start + Duration::from_secs(901),
            &settings,
            Some(CALM),
            ticks(4)
        ),
        Some(Pressure::Cpu)
    );
    assert_eq!(
        monitor.reason(
            start + Duration::from_secs(902),
            &settings,
            Some(CALM),
            Err(anyhow::anyhow!("unavailable"))
        ),
        None
    );
    assert!(monitor.cpu.since.is_none());
    assert_eq!(
        monitor.reason(
            start + Duration::from_secs(903),
            &settings,
            Some(CRITICAL),
            ticks(5)
        ),
        Some(Pressure::Memory)
    );
}

#[test]
fn warnings_follow_enabled_protections_and_sustained_cpu() {
    let start = Instant::now();
    let mut settings = crate::config::overload::Overload {
        poll_seconds: 100,
        ..Default::default()
    };
    let mut monitor = Monitor::default();
    let signals = |warnings: Vec<(Signal, crate::warnings::Warning)>| {
        warnings
            .into_iter()
            .map(|(signal, _)| signal)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        signals(monitor.warnings(start, &settings, Some(WARNING), false)),
        [Signal::Memory]
    );
    assert!(monitor.warnings(start, &settings, None, false).is_empty());
    settings.memory.enabled = false;
    assert!(
        monitor
            .warnings(start, &settings, Some(WARNING), false)
            .is_empty()
    );
    settings.memory.enabled = true;
    settings.warning.enabled = false;
    assert!(
        monitor
            .warnings(start, &settings, Some(WARNING), false)
            .is_empty()
    );
    settings.warning.enabled = true;

    // CPU warns only while its protection is enabled, after the warning
    // threshold has held for its sustained time.
    settings.cpu.enabled = true;
    monitor.cpu_used = Some(85.0);
    assert!(
        monitor
            .warnings(start, &settings, Some(CALM), false)
            .is_empty()
    );
    let sustained = Duration::from_secs(settings.warning.cpu_sustained_seconds);
    assert_eq!(
        signals(monitor.warnings(start + sustained, &settings, Some(CALM), false)),
        [Signal::Cpu]
    );
    monitor.cpu_used = Some(79.0);
    assert!(
        monitor
            .warnings(start + sustained * 2, &settings, Some(CALM), false)
            .is_empty()
    );
}

#[test]
fn pressure_must_be_continuous_and_restart_after_recovery() {
    let start = Instant::now();
    let mut pressure = Sustained::default();
    assert!(!pressure.observe(start, true, 30));
    assert!(!pressure.observe(start + Duration::from_secs(29), true, 30));
    assert!(!pressure.observe(start + Duration::from_secs(30), false, 30));
    assert!(!pressure.observe(start + Duration::from_secs(31), true, 30));
    assert!(!pressure.observe(start + Duration::from_secs(60), true, 30));
    assert!(pressure.observe(start + Duration::from_secs(61), true, 30));
}

#[test]
fn recovery_publishes_a_sustained_window_and_revokes_on_unsafe_readings() {
    let start = Instant::now();
    let settings = crate::config::overload::Overload::default();
    let sustained = Duration::from_secs(settings.recovery.sustained_seconds);
    let mut monitor = Monitor::default();
    assert_eq!(
        monitor.recovery(start, &settings, 0, Some(CALM), true),
        Recovery::Publish(None)
    );
    assert_eq!(
        monitor.recovery(start + sustained, &settings, 0, Some(CALM), true),
        Recovery::Publish(Some(0))
    );
    // A failed memory reading is unsafe, and turning unsafe revokes.
    assert_eq!(
        monitor.recovery(start + sustained * 2, &settings, 0, None, true),
        Recovery::Revoke
    );
    assert_eq!(
        monitor.recovery(start + sustained * 3, &settings, 1, Some(CALM), false),
        Recovery::Publish(None)
    );
}

#[test]
fn a_new_recovery_epoch_restarts_the_window_without_revoking() {
    let start = Instant::now();
    let settings = crate::config::overload::Overload::default();
    let sustained = Duration::from_secs(settings.recovery.sustained_seconds);
    let mut monitor = Monitor::default();
    monitor.recovery(start, &settings, 0, Some(CALM), true);
    assert_eq!(
        monitor.recovery(start + sustained, &settings, 1, Some(CALM), true),
        Recovery::Publish(None)
    );
    assert_eq!(
        monitor.recovery(start + sustained * 2, &settings, 1, Some(CALM), true),
        Recovery::Publish(Some(1))
    );
}
