use super::*;

#[test]
fn a_run_restored_after_a_pause_reports_its_own_memory_alone() {
    let killed = Some(ScopeMemory {
        oom_kills: 2,
        peak: 1 << 30,
    });
    // A pause's report stays for an end that reports nothing more.
    let mut paused = ReportedMemory::default();
    paused.paused(killed);
    paused.finished(None);
    assert_eq!(paused.last(), killed);
    // A restored run's end reports its own, here none, not the pause's.
    let mut restored = ReportedMemory::default();
    restored.paused(killed);
    restored.started();
    restored.finished(None);
    assert_eq!(restored.last(), None);
}
