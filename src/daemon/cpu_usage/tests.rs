use super::*;

fn ticks(busy: u64, total: u64) -> anyhow::Result<CpuTicks> {
    Ok(CpuTicks { busy, total })
}

#[tokio::test(start_paused = true)]
async fn averages_cover_the_window_and_forget_gaps() {
    let mut usage = CpuUsage::default();
    let start = Instant::now();
    assert_eq!(usage.average(start, RETAINED), None);
    usage.record(start, ticks(0, 0));
    assert_eq!(usage.average(start, RETAINED), None);
    usage.record(start + SAMPLE_INTERVAL, ticks(100, 100));
    usage.record(start + SAMPLE_INTERVAL * 2, ticks(100, 200));
    let now = start + SAMPLE_INTERVAL * 2;
    // The window reaches back to the oldest reading inside it.
    assert_eq!(usage.average(now, SAMPLE_INTERVAL), Some(0.0));
    assert_eq!(usage.average(now, RETAINED), Some(50.0));
    // A stalled sampler authorizes nothing, and its next reading starts over.
    let late = now + STALE + SAMPLE_INTERVAL;
    assert_eq!(usage.average(late, RETAINED), None);
    usage.record(late, ticks(300, 400));
    assert_eq!(usage.average(late, RETAINED), None);
    usage.record(late, Err(anyhow::anyhow!("unavailable")));
    assert!(usage.samples.is_empty());
}

#[tokio::test(start_paused = true)]
async fn readings_older_than_the_longest_window_are_dropped() {
    let mut usage = CpuUsage::default();
    let start = Instant::now();
    let intervals = (RETAINED.as_secs() / SAMPLE_INTERVAL.as_secs()) as u32;
    for interval in 0..=intervals + 2 {
        usage.record(
            start + SAMPLE_INTERVAL * interval,
            ticks(0, interval.into()),
        );
    }
    assert_eq!(usage.samples.len(), intervals as usize + 1);
}
