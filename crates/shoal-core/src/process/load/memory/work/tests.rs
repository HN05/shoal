use super::*;

#[test]
fn a_slice_holds_what_reclaim_cannot_free_with_what_it_swapped() {
    let stat = "anon 6000\nfile 5000\nkernel 300\nshmem 1000\nfile_mapped 200\n";
    // 10000 in memory, of which 4000 is file cache reclaim may drop.
    assert_eq!(held("10000\n", stat, Some("2500\n")).unwrap(), 8500);
    assert_eq!(held("10000\n", stat, None).unwrap(), 6000);
    assert!(held("10000\n", "anon 6000\n", None).is_err());
}

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
