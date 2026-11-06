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
    place(
        directory.path(),
        &imported("p/new.jsonl", "a\n"),
        Existing::Continued,
    )
    .unwrap();
    assert_eq!(read("p/new.jsonl"), "a\n");
    // The session went on elsewhere and came back.
    place(
        directory.path(),
        &imported("p/new.jsonl", "a\nb\n"),
        Existing::Continued,
    )
    .unwrap();
    assert_eq!(read("p/new.jsonl"), "a\nb\n");
    // An older copy never replaces it, nor does one that went another way.
    for other in ["a\n", "a\nc\nd\n"] {
        let error = place(
            directory.path(),
            &imported("p/new.jsonl", other),
            Existing::Continued,
        );
        if other == "a\n" {
            assert!(error.is_err());
        } else {
            let error = error.unwrap_err().to_string();
            assert!(error.contains("differs"), "{error}");
        }
        assert_eq!(read("p/new.jsonl"), "a\nb\n");
    }
}

#[test]
fn a_copy_never_replaces_a_session_file_here() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("p/live.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "a\n").unwrap();
    let inode = |path: &Path| std::os::unix::fs::MetadataExt::ino(&fs::metadata(path).unwrap());
    let before = inode(&path);
    // Its session may still run here, appending to the file it has open.
    for contents in ["a\n", "a\nb\n", "x\n"] {
        place(
            directory.path(),
            &imported("p/live.jsonl", contents),
            Existing::Kept,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n");
        assert_eq!(inode(&path), before);
    }
    place(
        directory.path(),
        &imported("p/copied.jsonl", "c\n"),
        Existing::Kept,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(directory.path().join("p/copied.jsonl")).unwrap(),
        "c\n"
    );
}
