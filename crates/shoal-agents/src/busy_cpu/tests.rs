use super::*;

const SECOND: Duration = Duration::from_secs(1);
const GAP: Duration = Duration::from_secs(6);
const WINDOW: Duration = Duration::from_secs(10);

/// Cumulative ticks with `busy` of `total` busy, half of them niced.
fn ticks(busy: u64, total: u64) -> Option<CpuTicks> {
    Some(CpuTicks {
        used: busy / 2,
        niced: busy - busy / 2,
        total,
    })
}

#[test]
fn the_reading_spans_the_latest_window_once_samples_cover_it() {
    let cpu = BusyCpu::default();
    let start = Instant::now();
    // A sample every 2 seconds of 200 ticks, 98% busy for the first 10
    // seconds and 80% after.
    for n in 0..=10u32 {
        let busy = match u64::from(n) {
            n @ 0..=5 => 196 * n,
            n => 980 + 160 * (n - 5),
        };
        cpu.observe(
            start + 2 * SECOND * n,
            ticks(busy, 200 * u64::from(n)),
            GAP,
            WINDOW,
        );
        let percent = cpu.percent(start + 2 * SECOND * n, WINDOW, GAP);
        match n {
            0..5 => assert_eq!(percent, None, "{n}"),
            5 => assert_eq!(percent, Some(98.0)),
            _ => {}
        }
    }
    assert_eq!(cpu.percent(start + 20 * SECOND, WINDOW, GAP), Some(80.0));
}

#[test]
fn the_reading_expires_once_sampling_pauses() {
    let cpu = BusyCpu::default();
    let start = Instant::now();
    for n in 0..=2 {
        cpu.observe(
            start + 5 * SECOND * n,
            ticks(450 * u64::from(n), 500 * u64::from(n)),
            GAP,
            WINDOW,
        );
    }
    assert_eq!(
        cpu.percent(start + 10 * SECOND + GAP, WINDOW, GAP),
        Some(90.0)
    );
    assert_eq!(cpu.percent(start + 11 * SECOND + GAP, WINDOW, GAP), None);
}

#[test]
fn a_gap_or_a_failed_reading_starts_the_window_over() {
    let cpu = BusyCpu::default();
    let start = Instant::now();
    let observe = |seconds: u32, ticks| cpu.observe(start + SECOND * seconds, ticks, GAP, WINDOW);
    let percent = |seconds: u32| cpu.percent(start + SECOND * seconds, WINDOW, GAP);
    for n in 0..=2 {
        observe(5 * n, ticks(450 * u64::from(n), 500 * u64::from(n)));
    }
    assert_eq!(percent(10), Some(90.0));
    observe(15, None);
    assert_eq!(percent(15), None);
    // Samples a gap apart never span a window.
    observe(20, ticks(1800, 2000));
    observe(30, ticks(2700, 3000));
    assert_eq!(percent(30), None);
}
