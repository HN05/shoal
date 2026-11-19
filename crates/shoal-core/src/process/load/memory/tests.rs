use super::*;

#[test]
fn memory_uses_available_including_reclaimable_cache() {
    let reading = "MemTotal: 10000 kB\nMemFree: 1 kB\nMemAvailable: 500 kB\n";
    assert!(linux_memory(reading, 95).unwrap());
    assert!(linux_memory(&reading.replace("500", "499"), 95).unwrap());
    assert!(!linux_memory(&reading.replace("500", "501"), 95).unwrap());
    for reading in [
        "MemTotal: 0 kB\nMemAvailable: 0 kB",
        "MemTotal: 10 kB\nMemAvailable: 11 kB",
        "MemTotal: 10 kB",
        "MemTotal: 10 MB\nMemAvailable: 0 kB",
        "MemTotal: x kB\nMemAvailable: 0 kB",
    ] {
        assert!(linux_memory(reading, 95).is_err(), "{reading}");
    }
    assert_eq!(
        linux_memory_reading(reading).unwrap(),
        Memory {
            total: 10000 * 1024,
            available: 500 * 1024,
        }
    );
}

#[test]
fn this_machine_reports_its_memory() {
    let memory = memory().unwrap();
    assert!(memory.available <= memory.total, "{memory:?}");
}
