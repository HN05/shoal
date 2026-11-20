use super::*;

const LEVELS: MemoryLevels = MemoryLevels {
    stop: MemoryLevel {
        used_percent: 95,
        stall_percent: 10,
    },
    warning: MemoryLevel {
        used_percent: 85,
        stall_percent: 10,
    },
    recovery: MemoryLevel {
        used_percent: 80,
        stall_percent: 2,
    },
};

const CALM: Stall = Stall {
    some_avg10: 0.0,
    some_avg60: 0.0,
    full_avg10: 0.0,
};

/// A reading of 10000 kB of RAM and as much swap.
fn meminfo(available: u64, swap_free: u64) -> String {
    format!(
        "MemTotal: 10000 kB\nMemFree: 1 kB\nMemAvailable: {available} kB\nSwapTotal: 10000 kB\nSwapFree: {swap_free} kB\n"
    )
}

#[test]
fn memory_counts_swap_with_reclaimable_cache_available() {
    assert_eq!(
        linux_memory(&meminfo(500, 2000)).unwrap(),
        Memory {
            total: 20000 * 1024,
            available: 2500 * 1024,
        }
    );
    let without_swap = "MemTotal: 10000 kB\nMemAvailable: 500 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB";
    assert_eq!(
        linux_memory(without_swap).unwrap(),
        Memory {
            total: 10000 * 1024,
            available: 500 * 1024,
        }
    );
    for reading in [
        "MemTotal: 0 kB\nMemAvailable: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB",
        "MemTotal: 10 kB\nMemAvailable: 11 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB",
        "MemTotal: 10 kB\nMemAvailable: 1 kB\nSwapTotal: 1 kB\nSwapFree: 2 kB",
        "MemTotal: 10 kB\nMemAvailable: 1 kB",
        "MemTotal: 10 MB\nMemAvailable: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB",
        "MemTotal: x kB\nMemAvailable: 0 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB",
    ] {
        assert!(linux_memory(reading).is_err(), "{reading}");
    }
}

#[test]
fn stall_reads_both_lines_of_the_pressure_file() {
    let text = "some avg10=7.45 avg60=3.69 avg300=12.02 total=217359613\nfull avg10=4.85 avg60=5.98 avg300=8.70 total=146533902\n";
    assert_eq!(
        linux_stall(text).unwrap(),
        Stall {
            some_avg10: 7.45,
            some_avg60: 3.69,
            full_avg10: 4.85,
        }
    );
    for text in [
        "some avg10=7.45 avg60=3.69 avg300=12.02 total=1",
        "some avg10=7.45 avg300=12.02 total=1\nfull avg10=4.85 avg60=5.98",
        "some avg10=nan avg60=3.69\nfull avg10=4.85 avg60=5.98",
        "some avg10=100.01 avg60=3.69\nfull avg10=4.85 avg60=5.98",
        "some avg10=x avg60=3.69\nfull avg10=4.85 avg60=5.98",
    ] {
        assert!(linux_stall(text).is_err(), "{text}");
    }
}

#[test]
fn filling_swap_counts_though_ram_alone_stays_below_a_level() {
    let pressure = |available, swap_free| {
        let memory = linux_memory(&meminfo(available, swap_free)).unwrap();
        pressure(memory, None, &LEVELS)
    };
    // RAM alone is 80% used; with full swap, 90% of both is.
    let filled = pressure(2000, 0);
    assert!(filled.warning && !filled.safe);
    assert!(!filled.critical && !filled.stalled);
    // At 95% of both, it stops at once.
    assert!(pressure(1000, 0).critical);
    let calm = pressure(4000, 10000);
    assert!(!calm.warning && calm.safe && !calm.critical);
}

#[test]
fn stall_warns_stops_and_holds_recovery_apart_from_memory_in_use() {
    let memory = linux_memory(&meminfo(9000, 10000)).unwrap();
    let pressure = |stall| pressure(memory, Some(stall), &LEVELS);
    assert_eq!(
        pressure(CALM),
        MemoryPressure {
            critical: false,
            stalled: false,
            warning: false,
            safe: true,
        }
    );
    // Some tasks stalling over ten seconds warns.
    let some = pressure(Stall {
        some_avg10: 10.0,
        ..CALM
    });
    assert!(some.warning && !some.stalled && some.safe);
    // Every task stalling stops work once it holds, never at once.
    let full = pressure(Stall {
        full_avg10: 10.0,
        ..CALM
    });
    assert!(full.stalled && !full.critical);
    // Recovery waits for stall over a minute to fall below its level.
    let lingering = pressure(Stall {
        some_avg60: 2.0,
        ..CALM
    });
    assert!(!lingering.safe && !lingering.warning);
}

#[test]
fn this_machine_reports_its_memory() {
    let reading = memory(&LEVELS, None).unwrap();
    assert!(
        reading.memory.available <= reading.memory.total,
        "{reading:?}"
    );
}

#[test]
fn work_past_its_limit_presses_however_calm_the_machine() {
    // A test build reads no slice, so the sampled use counts.
    let work = |sampled| WorkLimit {
        limit: 1000,
        sampled,
    };
    let reading = memory(&LEVELS, Some(work(Some(2000)))).unwrap();
    assert_eq!(reading.work.map(|work| work.available), Some(0));
    assert!(reading.pressure.critical && reading.pressure.warning && !reading.pressure.safe);
    // Without a sample there is no reading to admit work on.
    assert!(memory(&LEVELS, Some(work(None))).is_err());
}
