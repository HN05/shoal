use std::fs;

use super::*;
use crate::transfer::ImportedSessionFile;

fn imported(path: &str, contents: &str) -> ImportedSessionFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    fs::write(file.path(), contents).unwrap();
    ImportedSessionFile {
        file: shoal_protocol::transcripts::SessionFile {
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
        &imported("p/new.jsonl", "a\n").carried(),
        Existing::Continued,
    )
    .unwrap();
    assert_eq!(read("p/new.jsonl"), "a\n");
    // The session went on elsewhere and came back.
    place(
        directory.path(),
        &imported("p/new.jsonl", "a\nb\n").carried(),
        Existing::Continued,
    )
    .unwrap();
    assert_eq!(read("p/new.jsonl"), "a\nb\n");
    // An older copy never replaces it, nor does one that went another way.
    for other in ["a\n", "a\nc\nd\n"] {
        let error = place(
            directory.path(),
            &imported("p/new.jsonl", other).carried(),
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
            &imported("p/live.jsonl", contents).carried(),
            Existing::Kept,
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\n");
        assert_eq!(inode(&path), before);
    }
    place(
        directory.path(),
        &imported("p/copied.jsonl", "c\n").carried(),
        Existing::Kept,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(directory.path().join("p/copied.jsonl")).unwrap(),
        "c\n"
    );
}

#[test]
fn a_fork_finds_a_session_placed_where_its_tool_keeps_them_by_default() {
    let (default, directory) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let store = transcripts::store(shoal_protocol::integrations::IntegrationKind::Claude).unwrap();
    let carried = default.path().join("p/s1.jsonl");
    fs::create_dir_all(carried.parent().unwrap()).unwrap();
    fs::write(&carried, "carried\n").unwrap();
    let (name, session) = (&"claude".into(), &"s1".into());
    let directories = (default.path(), directory.path());
    copy_session(store, directories, (name, session)).unwrap();
    let copied = directory.path().join("p/s1.jsonl");
    assert_eq!(fs::read_to_string(&copied).unwrap(), "carried\n");
    // The session carried again replaces the copy it continues.
    fs::write(&carried, "carried\nlater\n").unwrap();
    copy_session(store, directories, (name, session)).unwrap();
    assert_eq!(fs::read_to_string(&copied).unwrap(), "carried\nlater\n");
    // A copy that went its own way stays.
    fs::write(&copied, "here\n").unwrap();
    assert!(copy_session(store, directories, (name, session)).is_err());
    assert_eq!(fs::read_to_string(&copied).unwrap(), "here\n");
}
