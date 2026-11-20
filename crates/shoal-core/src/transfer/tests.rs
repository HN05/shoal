use crate::execution::recovery::RecordExt;
use crate::time::UnixTime;
use std::{fs, path::PathBuf};

use super::*;
use crate::execution::recovery::{LastSession, Record};
use shoal_ids::agent_session_ids::AgentSessionId;

#[test]
fn import_refuses_other_files_and_versions() {
    assert!(read_package(b"not an export\n").is_err());
    let bytes = format!(
        "{FORMAT} 0.0.1\n{}\n",
        manifest_json(&manifest_with_sessions())
    );
    let refused = read_package(bytes.as_bytes()).err().expect("refused");
    assert!(
        format!("{refused:#}").contains("Shoal 0.0.1"),
        "{refused:#}"
    );
    // A format number of an earlier package names no release.
    let bytes = format!("{FORMAT} 9\n{{}}\n");
    let refused = read_package(bytes.as_bytes()).err().expect("refused");
    assert!(
        format!("{refused:#}").contains("an earlier Shoal"),
        "{refused:#}"
    );
}

/// A package read back whole, with the directory its parts are in.
struct Read {
    manifest: Manifest,
    parts: Parts,
    _directory: tempfile::TempDir,
}

/// Read the package `bytes` as an import does.
fn read_package(mut bytes: &[u8]) -> Result<Read> {
    let directory = tempfile::tempdir()?;
    let manifest = read_manifest(&mut bytes)?;
    let parts = Parts::read(&mut bytes, &manifest, directory.path())?;
    Ok(Read {
        manifest,
        parts,
        _directory: directory,
    })
}

/// The header of a package this Shoal writes.
fn header() -> String {
    format!("{FORMAT} {VERSION}")
}

fn manifest_json(manifest: &Manifest) -> String {
    serde_json::to_string(manifest).unwrap()
}

#[test]
fn a_workspace_package_keeps_its_branch_or_starting_commit() {
    let manifest = manifest_with_sessions();
    let mut at_commit = manifest.clone();
    at_commit.space.record.kept_workspace_mut().unwrap().head =
        crate::model::Head::Commit(crate::model::StartingCommit {
            commit: "abc".into(),
            made_for: None,
        });
    for manifest in [manifest, at_commit] {
        let bytes = export_bytes(&header(), &manifest, &[]);
        let import = read_package(&bytes).unwrap();
        let kept = import.manifest.space.record.kept_workspace().unwrap();
        assert_eq!(
            kept.head,
            manifest.space.record.kept_workspace().unwrap().head
        );
    }
}

#[test]
fn a_chat_package_holds_its_removal_record_and_continues_its_session() {
    let record: RemovedSpace = serde_json::from_value(serde_json::json!({
        "workspace_id": "c1",
        "repository_id": "r1",
        "name": "plan-cache",
        "parent": {"id": "lead", "name": "lead", "kind": "chat"},
        "links": {
            "issues": [{"kind": "issue", "url": "https://forge.example/team/repo/issues/12", "title": "Plan the cache"}],
            "prs": [{"kind": "pr", "url": "https://forge.example/team/repo/pulls/14", "title": "Fix it"}],
            "opened": [],
            "related": [{"kind": "issue", "url": "https://forge.example/team/repo/issues/9", "title": null}],
        },
        "last_sessions": [{"execution": "e1", "agent": "claude", "session": "s1", "mode": "plan"}],
        "cause": "manual",
        "removed_at": 1,
        "expires_at": 2,
        "chat": {"mode": "plan"},
    }))
    .unwrap();
    let mut manifest = Manifest {
        remote_url: None,
        sessions: Some(recorded_sessions(Some("m1".into()), &record)),
        space: PackagedSpace {
            prerequisite: None,
            bundle: None,
            record: moved(record.clone()),
        },
        session_files: vec![session_file("p/s1.jsonl", 3)],
        copy: None,
        carried: Vec::new(),
    };
    let bytes = export_bytes(&header(), &manifest, &["abc"]);
    let import = read_package(&bytes).unwrap();
    assert!(import.parts.bundles.is_empty());
    // The record comes back whole, under its ID and below its parent, with
    // its links' relations, for the import to restore.
    let packaged = &import.manifest.space.record;
    assert_eq!(
        (packaged.space_id.as_str(), packaged.name.as_str()),
        ("c1", "plan-cache")
    );
    assert_eq!(packaged.parent, record.parent);
    assert_eq!(packaged.links, record.links);
    let kept = packaged.chat().unwrap();
    assert_eq!(kept.mode.as_str(), "plan");
    // Another machine continues the session its files carry, in the
    // record it imports.
    let sessions = import.manifest.sessions_on(Some("m2"), None);
    let imported = import.manifest.space.record_with(sessions);
    let session = (imported.last_sessions.first()).and_then(|last| last.session.as_ref());
    assert_eq!(session.map(AgentSessionId::as_str), Some("s1"));
    // Without its files, another machine starts a new session.
    manifest.session_files.clear();
    assert_eq!(manifest.sessions_on(Some("m2"), None).reported().count(), 0);
    assert_eq!(manifest.sessions_on(Some("m1"), None).reported().count(), 1);
}

/// The bytes of an export of `manifest`'s format carrying `contents` for its
/// session files.
fn export_bytes(header: &str, manifest: &Manifest, contents: &[&str]) -> Vec<u8> {
    let mut bytes = format!("{header}\n{}\n", manifest_json(manifest));
    for content in contents {
        bytes.push_str(content);
    }
    bytes.into_bytes()
}

fn session_file(path: &str, size: u64) -> SessionFile {
    SessionFile {
        agent: "claude".into(),
        session: "s1".into(),
        path: path.into(),
        size,
    }
}

#[test]
fn import_reads_session_files_and_refuses_paths_outside_their_directory() {
    let mut manifest = manifest_with_sessions();
    manifest.session_files = vec![session_file("p/s1.jsonl", 3), session_file("p/s1/a", 2)];
    let bytes = export_bytes(&header(), &manifest, &["abc", "de"]);
    let import = read_package(&bytes).unwrap();
    let read: Vec<_> = import
        .parts
        .sessions
        .iter()
        .map(|imported| {
            let contents = fs::read_to_string(imported.contents.path()).unwrap();
            (imported.file.path.to_str().unwrap().to_owned(), contents)
        })
        .collect();
    assert_eq!(
        read,
        [
            ("p/s1.jsonl".to_owned(), "abc".to_owned()),
            ("p/s1/a".to_owned(), "de".to_owned())
        ]
    );
    let short = export_bytes(&header(), &manifest, &["abc", "d"]);
    assert!(read_package(&short).is_err());
    for path in ["../escape", "/absolute"] {
        manifest.session_files = vec![session_file(path, 1)];
        let bytes = export_bytes(&header(), &manifest, &["x"]);
        assert!(read_package(&bytes).is_err(), "{path}");
    }
}

#[test]
fn import_reads_the_bundle_of_each_packaged_workspace_after_the_session_files() {
    let mut manifest = manifest_with_sessions();
    manifest.session_files = vec![session_file("p/s1.jsonl", 3)];
    manifest.space.bundle = Some(2);
    let mut child = manifest.space.clone();
    child.record.space_id = "child".into();
    child.bundle = Some(4);
    let mut chat = manifest.space.clone();
    chat.record.space_id = "unbundled".into();
    chat.bundle = None;
    manifest.carried = [chat, child]
        .into_iter()
        .map(|space| CarriedSpace {
            remote_url: None,
            space,
            sessions: Sessions::default(),
        })
        .collect();
    let bytes = export_bytes(&header(), &manifest, &["abc", "de", "fghi"]);

    let import = read_package(&bytes).unwrap();

    let bundles: Vec<(&str, String)> = (import.parts.bundles.iter())
        .map(|(id, bundle)| (id.as_str(), fs::read_to_string(bundle.path()).unwrap()))
        .collect();
    let own = manifest.space.record.space_id.as_str();
    let mut expected = vec![("child", "fghi".to_owned()), (own, "de".to_owned())];
    expected.sort();
    assert_eq!(bundles, expected);
    let trailing = export_bytes(&header(), &manifest, &["abc", "de", "fghi", "j"]);
    assert!(read_package(&trailing).is_err());
}

/// An export's manifest with one stopped agent and a last session, both with
/// sessions, made on machine `m1`.
fn manifest_with_sessions() -> Manifest {
    let record: Record = serde_json::from_value(serde_json::json!({
        "agent": "claude",
        "session": "s1",
        "origin": {"host": "herdr", "place": "tab"},
        "mode": "worker",
    }))
    .unwrap();
    let agent = StoppedAgent {
        execution: "execution".into(),
        record,
        stopped_at: UnixTime::from_seconds(1),
    };
    let last = LastSession {
        narrowing: Default::default(),
        mode: "worker".into(),
        model: Default::default(),
        execution: "quit".into(),
        agent: "codex".into(),
        session: Some("s2".into()),
        moved: false,
        copied: None,
        origin: Some(
            serde_json::from_value(serde_json::json!({"host": "herdr", "place": "tab"})).unwrap(),
        ),
        stop: None,
        converted: false,
    };
    let workspace: RemovedSpace = serde_json::from_value(serde_json::json!({
        "workspace_id": "w1",
        "repository_id": "r1",
        "name": "topic",
        "checkpoint": {
            "workspace_id": "w1",
            "repository_id": "r1",
            "number": 1,
            "cause": "removal",
            "branch": "topic",
            "head": "h",
            "commit": "h",
            "files_omitted": false,
            "created_at": 1,
            "expires_at": 2,
        },
        "base_ref": null,
        "base_commit": null,
        "branch": "topic",
        "links": {"issues": [], "prs": [], "opened": [], "related": []},
        "agents": [agent],
        "last_sessions": [last],
        "cause": "manual",
        "removed_at": 1,
        "expires_at": 2,
    }))
    .unwrap();
    Manifest {
        remote_url: None,
        sessions: Some(Sessions {
            machine: Some("m1".into()),
            agents: workspace.agents.clone(),
            last_sessions: workspace.last_sessions.clone(),
        }),
        space: PackagedSpace {
            prerequisite: None,
            bundle: None,
            record: moved(workspace),
        },
        session_files: Vec::new(),
        copy: None,
        carried: Vec::new(),
    }
}

#[test]
fn an_import_keeps_sessions_on_the_exporting_machine_or_when_asked() {
    let mut manifest = manifest_with_sessions();
    let here = manifest.sessions_on(Some("m1"), None);
    assert_eq!(here.agents, manifest.sessions.as_ref().unwrap().agents);
    assert!(!here.agents[0].record.moved);
    let place =
        |origin: Option<&crate::sessions::Origin>| origin.and_then(|origin| origin.place.clone());
    assert_eq!(
        place(here.agents[0].record.origin.as_ref()).as_deref(),
        Some("tab")
    );
    let [last] = here.last_sessions.as_slice() else {
        panic!("one last session: {:?}", here.last_sessions);
    };
    assert_eq!(last.execution, "quit");
    assert_eq!(
        (
            last.session.as_ref().map(AgentSessionId::as_str),
            last.moved
        ),
        (Some("s2"), false)
    );
    assert_eq!(place(last.origin.as_ref()).as_deref(), Some("tab"));

    for new in [
        manifest.sessions_on(Some("m2"), None),
        manifest.sessions_on(None, None),
        manifest.sessions_on(Some("m1"), Some(false)),
    ] {
        assert!(new.agents[0].record.moved);
        assert_eq!(new.agents[0].record.session, None);
        assert!(new.last_sessions[0].moved);
    }

    // Another machine told to keep the sessions keeps no terminal place of
    // the exporting machine's.
    let kept = manifest.sessions_on(Some("m2"), Some(true));
    assert!(!kept.agents[0].record.moved);
    assert_eq!(kept.agents[0].record.session, Some("s1".into()));
    assert_eq!(kept.agents[0].record.origin, None);
    assert!(!kept.last_sessions[0].moved);
    assert_eq!(kept.last_sessions[0].origin, None);

    // So does an export from a machine without an ID.
    manifest.sessions.as_mut().unwrap().machine = None;
    assert!(manifest.sessions_on(None, None).agents[0].record.moved);
    let kept = manifest.sessions_on(None, Some(true));
    assert_eq!(kept.agents[0].record.session, Some("s1".into()));
    assert_eq!(kept.agents[0].record.origin, None);

    // Another machine keeps a session whose files the export carries.
    manifest.session_files = vec![session_file("p/s1.jsonl", 1)];
    let carried = manifest.sessions_on(Some("m2"), None);
    assert_eq!(carried.agents[0].record.session, Some("s1".into()));
    assert_eq!(carried.agents[0].record.origin, None);
    assert!(carried.last_sessions[0].moved);
    let new = manifest.sessions_on(Some("m2"), Some(false));
    assert!(new.agents[0].record.moved);
    manifest.session_files.clear();

    // An agent that never reported a session starts a new one either way.
    let sessions = manifest.sessions.as_mut().unwrap();
    sessions.last_sessions[0].session = None;
    let kept = manifest.sessions_on(None, Some(true));
    assert!(!kept.agents[0].record.moved);
    assert!(kept.last_sessions[0].moved);
}

#[test]
fn moved_records_keep_no_session_place_or_wrapper() {
    let record: Record = serde_json::from_value(serde_json::json!({
        "agent": "codex",
        "session": "s1",
        "origin": {"host": "herdr", "place": "tab"},
        "mode": "worker",
    }))
    .unwrap();
    assert!(!record.moved);
    let moved = record.moved();
    assert!(moved.moved);
    assert_eq!(
        (moved.session, moved.origin, moved.waiting),
        (None, None, None)
    );
}

#[test]
fn a_session_with_an_unreadable_file_is_left_out_with_a_warning() {
    let directory = tempfile::tempdir().unwrap();
    let source = |session: &str, name: &str| {
        let path = directory.path().join(name);
        fs::write(&path, name).unwrap();
        SessionSource {
            agent: "claude".into(),
            session: session.into(),
            path: name.into(),
            source: path,
        }
    };
    let kept = source("s1", "kept.jsonl");
    let readable = source("s2", "readable.jsonl");
    // Gone since it was found, which no privilege opens.
    let unreadable = source("s2", "unreadable.jsonl");
    fs::remove_file(&unreadable.source).unwrap();
    let (opened, warnings) = open_sessions(vec![kept, readable, unreadable]);
    let paths: Vec<_> = opened.iter().map(|(file, _)| file.path.clone()).collect();
    assert_eq!(paths, [PathBuf::from("kept.jsonl")]);
    assert_eq!(opened[0].0.size, "kept.jsonl".len() as u64);
    let [warning] = warnings.as_slice() else {
        panic!("one warning: {warnings:?}");
    };
    assert!(warning.contains("claude session s2"), "{warning}");
}

#[test]
fn a_copy_forks_its_agents_sessions_in_no_place_of_the_original() {
    let copied = Copied {
        earlier_files: true,
        held_elsewhere: false,
    };
    let manifest = Manifest {
        copy: Some(copied),
        ..manifest_with_sessions()
    };
    let here = manifest.sessions_on(Some("m1"), None);
    let agent = &here.agents[0].record;
    assert_eq!(agent.session, Some("s1".into()));
    assert_eq!((agent.origin.as_ref(), agent.copied), (None, Some(copied)));
    let last = &here.last_sessions[0];
    assert_eq!(last.session, Some("s2".into()));
    assert_eq!((last.origin.as_ref(), last.copied), (None, Some(copied)));
    // Without the session, the copy's agents start anew, still told it is
    // a copy.
    let new = manifest.sessions_on(Some("m2"), None);
    assert_eq!(new.agents[0].record.session, None);
    assert_eq!(new.agents[0].record.copied, Some(copied));
    assert_eq!(new.last_sessions[0].copied, Some(copied));
    // Each import runs them as executions of their own, apart from the
    // original's, which may still run.
    let original = manifest.sessions.as_ref().unwrap();
    for (sessions, other) in [(&here, &new), (&new, &here)] {
        assert_ne!(sessions.agents[0].execution, original.agents[0].execution);
        assert_ne!(sessions.agents[0].execution, other.agents[0].execution);
        assert_ne!(
            sessions.last_sessions[0].execution,
            original.last_sessions[0].execution
        );
    }
}
