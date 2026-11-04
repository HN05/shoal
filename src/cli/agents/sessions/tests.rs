use std::fs;

use super::*;

fn imported(path: &str, contents: &str) -> ImportedSessionFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), contents).unwrap();
    ImportedSessionFile {
        file: crate::transfer::SessionFile {
            agent: "claude".into(),
            session: "s1".into(),
            path: path.into(),
            size: contents.len() as u64,
        },
        contents: file,
    }
}

#[test]
fn placing_a_session_file_keeps_one_here_unless_the_import_continues_it() {
    let directory = tempfile::tempdir().unwrap();
    let read = |path: &str| fs::read_to_string(directory.path().join(path)).unwrap();
    place(directory.path(), &imported("p/new.jsonl", "a\n")).unwrap();
    assert_eq!(read("p/new.jsonl"), "a\n");
    // The session went on elsewhere and came back.
    place(directory.path(), &imported("p/new.jsonl", "a\nb\n")).unwrap();
    assert_eq!(read("p/new.jsonl"), "a\nb\n");
    // An older copy never replaces it, nor does one that went another way.
    for other in ["a\n", "a\nc\nd\n"] {
        let error = place(directory.path(), &imported("p/new.jsonl", other));
        if other == "a\n" {
            assert!(error.is_err());
        } else {
            let error = error.unwrap_err().to_string();
            assert!(error.contains("differs"), "{error}");
        }
        assert_eq!(read("p/new.jsonl"), "a\nb\n");
    }
}
