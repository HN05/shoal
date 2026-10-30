use std::{collections::BTreeMap, fs, path::PathBuf};

use super::*;
use crate::cli::agents::{claude::Claude, codex::Codex};

fn write(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "x").unwrap();
}

fn paths(files: &[&str]) -> Vec<PathBuf> {
    files.iter().map(PathBuf::from).collect()
}

#[test]
fn claude_sessions_are_the_transcript_and_its_directory_in_any_project() {
    let home = tempfile::tempdir().unwrap();
    let configured = BTreeMap::new();
    let environment = AgentEnvironment {
        home: home.path(),
        configured: &configured,
    };
    let directory = Claude.session_dir(&environment).unwrap().unwrap();
    assert_eq!(directory, home.path().join(".claude/projects"));
    assert!(Claude.session_files(&directory, "s1").unwrap().is_empty());
    write(&directory.join("-a/s1.jsonl"));
    write(&directory.join("-a/s1/subagents/one.jsonl"));
    write(&directory.join("-a/s2.jsonl"));
    write(&directory.join("-b/s1/tool-results/big.txt"));
    assert_eq!(
        Claude.session_files(&directory, "s1").unwrap(),
        paths(&[
            "-a/s1.jsonl",
            "-a/s1/subagents/one.jsonl",
            "-b/s1/tool-results/big.txt"
        ])
    );

    let configured = BTreeMap::from([(
        crate::env::CLAUDE_CONFIG_DIR.to_owned(),
        home.path().join("claude").into_os_string(),
    )]);
    let environment = AgentEnvironment {
        home: home.path(),
        configured: &configured,
    };
    assert_eq!(
        Claude.session_dir(&environment).unwrap().unwrap(),
        home.path().join("claude/projects")
    );
}

#[test]
fn codex_sessions_are_the_rollout_named_after_them() {
    let home = tempfile::tempdir().unwrap();
    let configured = BTreeMap::new();
    let environment = AgentEnvironment {
        home: home.path(),
        configured: &configured,
    };
    let directory = Codex.session_dir(&environment).unwrap().unwrap();
    assert_eq!(directory, home.path().join(".codex/sessions"));
    write(&directory.join("2026/10/30/rollout-2026-10-30T01-02-03-s1.jsonl"));
    write(&directory.join("2026/10/30/rollout-2026-10-30T01-02-03-xs1.jsonl"));
    write(&directory.join("2026/10/29/rollout-2026-10-29T01-02-03-s2.jsonl"));
    assert_eq!(
        Codex.session_files(&directory, "s1").unwrap(),
        paths(&["2026/10/30/rollout-2026-10-30T01-02-03-s1.jsonl"])
    );
}

#[test]
fn session_ids_name_one_file() {
    assert!(is_file_name("0f0e-11"));
    for id in ["", "..", "a/b", "/abs", "."] {
        assert!(!is_file_name(id), "{id}");
    }
}

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
