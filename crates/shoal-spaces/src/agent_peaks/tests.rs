use super::*;

const GIB: u64 = 1 << 30;

fn space() -> SpaceId {
    "api".into()
}

#[test]
fn a_rise_of_a_build_s_worth_during_a_call_is_told_once_an_hour() {
    let mut peaks = AgentPeaks::default();
    let start = Instant::now();
    peaks.observe([(space(), GIB)]);
    // The first call has no base to rise from.
    assert_eq!(peaks.rise_after_call(&space(), start), None);
    peaks.observe([(space(), 5 * GIB)]);
    peaks.observe([(space(), 2 * GIB)]);
    assert_eq!(peaks.rise_after_call(&space(), start), Some(4 * GIB));
    // The next call starts from what it holds then.
    peaks.observe([(space(), 6 * GIB)]);
    assert_eq!(peaks.rise_after_call(&space(), start), None);
    peaks.observe([(space(), 9 * GIB)]);
    let later = start + NUDGE_INTERVAL;
    assert_eq!(peaks.rise_after_call(&space(), later), Some(3 * GIB));
    // A small rise tells nothing.
    peaks.observe([(space(), 10 * GIB)]);
    let much_later = later + NUDGE_INTERVAL;
    assert_eq!(peaks.rise_after_call(&space(), much_later), None);
}
