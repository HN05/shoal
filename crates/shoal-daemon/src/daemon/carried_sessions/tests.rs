use std::path::PathBuf;

use shoal_protocol::transcripts::SessionFile;

use super::{join_files, split_files};

/// A file of session `parent` at `path` in its tool's directory.
fn file(path: &str) -> SessionFile {
    SessionFile {
        agent: "claude".parse().unwrap(),
        session: "parent".into(),
        path: path.into(),
        size: 0,
    }
}

#[test]
fn a_session_s_files_arrive_as_they_were_sent() {
    let root = tempfile::tempdir().unwrap();
    let sent = [
        ("projects/x/parent.jsonl", "{}\n{}\n"),
        ("projects/x/parent/a.txt", ""),
    ];
    let sources: Vec<_> = (sent.iter().enumerate())
        .map(|(index, (path, contents))| {
            let source = root.path().join(format!("source-{index}"));
            std::fs::write(&source, contents).unwrap();
            (file(path), source)
        })
        .collect();
    let joined = root.path().join("joined");
    let files = join_files(&sources, &joined).unwrap();
    let received = root.path().join("received");
    std::fs::create_dir(&received).unwrap();
    let carried = split_files(&joined, files, &received).unwrap();
    let arrived: Vec<_> = (carried.iter())
        .map(|carried| {
            let contents = std::fs::read_to_string(&carried.contents).unwrap();
            (carried.file.path.clone(), contents)
        })
        .collect();
    let sent: Vec<_> = (sent.iter())
        .map(|(path, contents)| (PathBuf::from(path), contents.to_string()))
        .collect();
    assert_eq!(arrived, sent);
}

#[test]
fn files_that_arrive_shorter_or_longer_than_listed_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let joined = root.path().join("joined");
    std::fs::write(&joined, "four").unwrap();
    for size in [3, 5] {
        let received = root.path().join(format!("received-{size}"));
        std::fs::create_dir(&received).unwrap();
        let listed = vec![SessionFile {
            size,
            ..file("projects/x/parent.jsonl")
        }];
        assert!(split_files(&joined, listed, &received).is_err(), "{size}");
    }
}
