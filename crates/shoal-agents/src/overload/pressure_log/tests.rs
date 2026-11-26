use super::*;
use crate::process::load::Memory;

const CALM: MemoryPressure = MemoryPressure {
    critical: false,
    stalled: false,
    warning: false,
    safe: true,
};

const WARNING: MemoryPressure = MemoryPressure {
    warning: true,
    safe: false,
    ..CALM
};

const STALLED: MemoryPressure = MemoryPressure {
    stalled: true,
    ..WARNING
};

#[test]
fn a_level_repeats_only_after_a_while_and_a_higher_one_at_once() {
    let start = Instant::now();
    let second = Duration::from_secs(1);
    let mut log = PressureLog::default();
    assert_eq!(log.due(start, WARNING), Some(Level::Warning));
    assert_eq!(log.due(start + second, WARNING), None);
    assert_eq!(log.due(start + second * 2, STALLED), Some(Level::Stop));
    assert_eq!(log.due(start + second * 3, WARNING), None);
    assert_eq!(
        log.due(start + second * 2 + REPEAT, WARNING),
        Some(Level::Warning)
    );
    // Calm readings end the stretch, so the next pressure logs at once.
    assert_eq!(log.due(start + REPEAT * 2, CALM), None);
    assert_eq!(
        log.due(start + REPEAT * 2 + second, WARNING),
        Some(Level::Warning)
    );
}

#[test]
fn the_log_line_carries_the_figures_pressure_stands_on() {
    let reading = MemoryReading {
        memory: Memory {
            total: 1000,
            available: 70,
        },
        stall: Some(Stall {
            some_avg10: 45.5,
            some_avg60: 12.0,
            full_avg10: 30.25,
        }),
        work: Some(WorkMemory::Read {
            memory: Memory {
                total: 500,
                available: 100,
            },
            stall: None,
        }),
        pressure: STALLED,
    };
    let text = describe(&reading);
    for figure in [
        "93.0% in use",
        "some 45.50% full 30.25%",
        "80.0% of its limit, no stall reading",
    ] {
        assert!(text.contains(figure), "{figure} missing from {text}");
    }
}
