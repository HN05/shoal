use super::*;

#[test]
fn work_reads_against_its_limit() {
    let work = WorkLimit {
        limit: 1000,
        sampled: None,
    };
    let memory = work.against(400);
    assert_eq!((memory.total, memory.available), (1000, 600));
    assert!(memory.used_at_least(40) && !memory.used_at_least(41));
    // Work past its limit leaves nothing.
    assert_eq!(work.against(1200).available, 0);
}
