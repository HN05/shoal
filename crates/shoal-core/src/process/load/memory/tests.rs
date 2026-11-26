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

/// A reading of 10000 kB of RAM.
fn meminfo(available: u64) -> String {
    format!(
        "MemTotal: 10000 kB\nMemFree: 1 kB\nMemAvailable: {available} kB\nSwapTotal: 18000 kB\nSwapFree: 18000 kB\n"
    )
}

const SWAPS_HEADER: &str = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n";

/// A swap file of 10000 kB with `used` of it in use, beside a zram device
/// of 8000 kB.
fn swaps(used: u64) -> String {
    format!(
        "{SWAPS_HEADER}/dev/zram0                              partition\t8000\t\t8000\t\t100\n/swapfile                               file\t\t10000\t\t{used}\t\t-2\n"
    )
}

#[test]
fn memory_counts_swap_on_disk_with_reclaimable_cache_available() {
    assert_eq!(
        linux_memory(&meminfo(500), &swaps(8000)).unwrap(),
        Memory {
            total: 20000 * 1024,
            available: 2500 * 1024,
        }
    );
    // A machine whose only swap is zram counts its RAM alone.
    let zram = format!("{SWAPS_HEADER}/dev/zram0 partition 8000 4000 100\n");
    for swaps in [zram.as_str(), SWAPS_HEADER, ""] {
        assert_eq!(
            linux_memory(&meminfo(500), swaps).unwrap(),
            Memory {
                total: 10000 * 1024,
                available: 500 * 1024,
            },
            "{swaps}"
        );
    }
    for reading in [
        "MemTotal: 0 kB\nMemAvailable: 0 kB",
        "MemTotal: 10 kB\nMemAvailable: 11 kB",
        "MemTotal: 10 kB",
        "MemTotal: 10 MB\nMemAvailable: 0 kB",
        "MemTotal: x kB\nMemAvailable: 0 kB",
    ] {
        assert!(linux_memory(reading, "").is_err(), "{reading}");
    }
    for swaps in [
        format!("{SWAPS_HEADER}/swapfile file 10 11 -2"),
        format!("{SWAPS_HEADER}/swapfile file 10"),
        format!("{SWAPS_HEADER}/swapfile file x 0 -2"),
    ] {
        assert!(linux_memory(&meminfo(500), &swaps).is_err(), "{swaps}");
    }
}

#[test]
fn only_zram_devices_count_as_zram() {
    assert!(is_zram("/dev/zram0") && is_zram("/dev/zram12"));
    for name in ["/dev/zram", "/dev/zramx", "/dev/sda2", "/swap/zram0"] {
        assert!(!is_zram(name), "{name}");
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
    let pressure = |available, swap_used| {
        let memory = linux_memory(&meminfo(available), &swaps(swap_used)).unwrap();
        pressure(memory, None, &LEVELS)
    };
    // RAM alone is 80% used; with full swap, 90% of both is.
    let filled = pressure(2000, 10000);
    assert!(filled.warning && !filled.safe);
    assert!(!filled.critical && !filled.stalled);
    // At 95% of both, it stops at once.
    assert!(pressure(1000, 10000).critical);
    let calm = pressure(4000, 0);
    assert!(!calm.warning && calm.safe && !calm.critical);
}

#[test]
fn stall_warns_stops_and_holds_recovery_apart_from_memory_in_use() {
    let memory = linux_memory(&meminfo(9000), &swaps(0)).unwrap();
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
    let sampled = SampledWork {
        all: 2000,
        outside_slice: 2000,
    };
    let reading = memory(&LEVELS, Some(work(Some(sampled)))).unwrap();
    let Some(WorkMemory::Read { memory: used, .. }) = reading.work else {
        panic!("{reading:?}");
    };
    assert_eq!(used.available, 0);
    assert!(reading.pressure.critical && reading.pressure.warning && !reading.pressure.safe);
    // Without a sample the work is unread, and nothing is safe to recover
    // on, but the machine's own pressure stands.
    let unread = memory(&LEVELS, Some(work(None))).unwrap();
    let machine = memory(&LEVELS, None).unwrap();
    assert_eq!(unread.work, Some(WorkMemory::Unread));
    assert!(!unread.pressure.safe);
    assert_eq!(unread.pressure.critical, machine.pressure.critical);
}
