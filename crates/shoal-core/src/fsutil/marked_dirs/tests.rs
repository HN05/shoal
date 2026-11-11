use super::*;

#[test]
fn marked_directories_are_never_found_unmarked_or_claimed() {
    let temp = tempfile::tempdir().unwrap();
    for attempt in 0..20 {
        let root = temp.path().join(format!("repo-{attempt}/.caches"));
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    start.wait();
                    create_marked_dir(&root, "repo").unwrap();
                    assert!(is_marked_dir(&root, "repo"));
                });
            }
        });
    }
    // A directory someone else created is never claimed.
    let theirs = temp.path().join("theirs");
    fs::create_dir(&theirs).unwrap();
    create_marked_dir(&theirs, "repo").unwrap();
    assert!(!is_marked_dir(&theirs, "repo"));
    assert_eq!(fs::read_dir(&theirs).unwrap().count(), 0);
}
