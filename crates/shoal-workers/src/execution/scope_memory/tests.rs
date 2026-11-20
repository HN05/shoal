use super::*;

/// Write a scope's memory files: `events` kills and `held` bytes beside a
/// page cache of a gibibyte.
fn write_scope(scope: &std::path::Path, kills: u64, held: u64) {
    let cache = 1 << 30;
    let files = [
        ("memory.events", format!("oom 1\noom_kill {kills}\n")),
        ("memory.current", format!("{}\n", held + cache)),
        (
            "memory.stat",
            format!("anon {held}\nfile {cache}\nshmem 0\n"),
        ),
    ];
    for (name, text) in files {
        std::fs::write(scope.join(name), text).unwrap();
    }
}

#[test]
fn a_scope_keeps_its_highest_reading_and_its_last_count_once_gone() {
    let scope = tempfile::tempdir().unwrap();
    let mut read = Read {
        scope: Some(scope.path().to_owned()),
        ..Read::default()
    };
    write_scope(scope.path(), 0, 3 << 30);
    assert!(read.update());
    write_scope(scope.path(), 2, 1 << 30);
    assert!(read.update());
    assert_eq!(
        read.memory,
        ScopeMemory {
            oom_kills: 2,
            peak: 3 << 30
        }
    );
    // The scope goes with its last process; what was read stays.
    let gone = scope.path().to_owned();
    drop(scope);
    assert!(!gone.exists());
    assert!(!read.update());
    assert_eq!(read.memory.oom_kills, 2);
}
