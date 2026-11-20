use super::*;

#[test]
fn unread_containers_warn_once_each_time_they_begin_to_hold_back_work() {
    let mut unread = UnreadContainers::default();
    assert!(!unread.begins(false));
    assert!(unread.begins(true));
    // While they go on holding work back, nothing more.
    assert!(!unread.begins(true));
    // Read again, or no longer holding back, then holding back once more.
    assert!(!unread.begins(false));
    assert!(unread.begins(true));
}

#[test]
fn unread_work_holds_back_starts_only_under_a_limit_protection_reads() {
    let mut config = Config::default();
    assert!(!unread_work_holds(&config));
    config.machine.memory_limit = Some(MemoryLimit::Bytes(8 << 30));
    assert!(unread_work_holds(&config));
    config.machine.overload.memory.enabled = false;
    assert!(!unread_work_holds(&config));
}
