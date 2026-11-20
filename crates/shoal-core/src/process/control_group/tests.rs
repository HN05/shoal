use super::*;

#[test]
fn a_group_holds_what_reclaim_cannot_free_with_what_it_swapped() {
    let stat = "anon 6000\nfile 5000\nkernel 300\nshmem 1000\nfile_mapped 200\n";
    // 10000 in memory, of which 4000 is file cache reclaim may drop.
    assert_eq!(held_from("10000\n", stat, Some("2500\n")).unwrap(), 8500);
    assert_eq!(held_from("10000\n", stat, None).unwrap(), 6000);
    assert!(held_from("10000\n", "anon 6000\n", None).is_err());
}

#[test]
fn a_group_reads_its_kills_until_it_is_gone() {
    let group = tempfile::tempdir().unwrap();
    let events = "low 0\nhigh 12\nmax 3\noom 2\noom_kill 2\noom_group_kill 1\n";
    std::fs::write(group.path().join("memory.events"), events).unwrap();
    assert_eq!(oom_kills(group.path()).unwrap(), Some(2));
    let gone = group.path().join("gone.scope");
    assert_eq!(oom_kills(&gone).unwrap(), None);
    assert_eq!(held(&gone).unwrap(), None);
}
