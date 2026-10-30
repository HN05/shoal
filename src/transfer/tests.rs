use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::*;
use crate::{
    daemon::workspace::Manager,
    execution::recovery::{self, LastSession, Record},
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{commit, git, manager, repository},
};

/// A registered repository pushed to a bare `origin`.
async fn exporter() -> (
    tempfile::TempDir,
    Arc<Manager>,
    Repository,
    std::path::PathBuf,
) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let origin = root.path().join("origin.git");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(
        &path,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&path, &["push", "-q", "-u", "origin", "main"]);
    git(&path, &["remote", "set-head", "origin", "main"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (root, manager, repo, origin)
}

/// Another Shoal with its own clone of `origin`.
async fn importer(origin: &Path) -> (tempfile::TempDir, Arc<Manager>, Repository) {
    let (root, manager) = manager().await;
    git(
        root.path(),
        &["clone", "-q", origin.to_str().unwrap(), "repo"],
    );
    let repo = manager
        .register_repository(
            root.path().join("repo").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    (root, manager, repo)
}

/// Export `workspace`, removing it, and read the file back.
async fn export_removed(manager: &Manager, workspace: &str) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("workspace.shoal");
    let exported = manager
        .export_workspace(
            workspace,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
        )
        .await
        .unwrap();
    assert!(exported.removal.is_some());
    // The daemon writes new files only.
    let error = manager
        .export_workspace(
            workspace,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("write"), "{error:#}");
    fs::read(file).unwrap()
}

async fn import(
    manager: &Manager,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    manager
        .import_workspace(
            &repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            import.manifest.prerequisite.as_deref(),
            import.manifest.workspace,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn import_opens_the_exported_branch_with_its_files_and_agents() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    // The default branch moves after the importer cloned it, so the import
    // fetches what the bundle leaves out.
    fs::write(source_repo.path.join("later"), "later\n").unwrap();
    commit(&source_repo.path, "later");
    git(&source_repo.path, &["push", "-q", "origin", "main"]);
    let workspace = exporting
        .create_workspace(&source_repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    let status = git(&workspace.path, &["status", "--porcelain"]);
    recovery::save_session(&exporting.paths, &workspace.id, "stopped-agent", "s1").unwrap();
    recovery::save_record(
        &exporting.paths,
        &workspace.id,
        "stopped-agent",
        "claude",
        Some("overload"),
        None,
    )
    .unwrap();

    let file = export_removed(&exporting, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert_eq!(read.manifest.remote_url.as_deref(), origin.to_str());
    assert!(read.manifest.bundle);
    // The exporting machine's import can resume the session it recorded.
    let sessions = read.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.machine, crate::process::machine::id());
    assert_eq!(sessions.agents[0].record.session.as_deref(), Some("s1"));
    let restored = import(&importing, &target_repo, &file).await;

    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let path = &restored.opened.workspace.path;
    assert!(path.starts_with(target_repo.path.parent().unwrap()));
    assert_eq!(restored.opened.workspace.branch, "topic");
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(path, &["status", "--porcelain"]), status);
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    assert!(importing.removed_workspaces().await.unwrap().is_empty());
    let checkpoints = importing
        .checkpoints(&restored.opened.workspace.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].repository_id, target_repo.id);
    // The stopped agent starts a new session on another machine.
    let saved = recovery::Saved::load(&importing.paths, &restored.opened.workspace.id).unwrap();
    assert_eq!(saved.agents.len(), 1);
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    assert_eq!(record.agent, "claude");
    assert!(record.moved);
    assert_eq!(record.session, None);
}

#[tokio::test]
async fn import_writes_nothing_while_the_repository_moves() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let file = export_removed(&exporting, &workspace.id).await;

    // A move that has not finished: its checkout may be half copied.
    let id = target_repo.id.clone();
    importing
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO repository_moves
                    (repository_id,source,destination,source_id,destination_id,states)
                    VALUES (?1,'/old','/new','old','new','{}')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let import = Import::read(&mut &file[..]).unwrap();
    let error = importing
        .import_workspace(
            &target_repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            import.manifest.prerequisite.as_deref(),
            import.manifest.workspace,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("repository move is incomplete"),
        "{error:#}"
    );
    assert!(importing.removed_workspaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn export_leaves_out_a_checkpoint_the_default_branch_has() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "empty".into(), None, None, None)
        .await
        .unwrap();
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);

    let file = export_removed(&exporting, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert!(!read.manifest.bundle);
    assert_eq!(read.manifest.prerequisite.as_deref(), Some(head.trim()));
    let restored = import(&importing, &target_repo, &file).await;
    assert_eq!(
        git(&restored.opened.workspace.path, &["rev-parse", "HEAD"]),
        head
    );
}

#[tokio::test]
async fn export_the_daemon_cannot_write_keeps_the_workspace() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "kept".into(), None, None, None)
        .await
        .unwrap();
    let blocked = source.path().join("file");
    fs::write(&blocked, "").unwrap();

    let error = exporting
        .export_workspace(
            &workspace.id,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            blocked.join("workspace.shoal"),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("the Shoal daemon cannot write to"),
        "{error:#}"
    );
    assert!(exporting.workspace(&workspace.id).await.is_ok());
    assert!(workspace.path.exists());
}

#[test]
fn import_refuses_other_files_and_newer_formats() {
    let error = Import::read(&mut &b"not an export\n"[..]).err().unwrap();
    assert_eq!(error.to_string(), "not a Shoal workspace export");
    let error = Import::read(&mut &b"shoal workspace 3\n{}\n"[..])
        .err()
        .unwrap();
    assert!(error.to_string().contains("newer format"), "{error}");
}

/// The bytes of an export of `manifest`'s format carrying `contents` for its
/// session files.
fn export_bytes(header: &str, manifest: &Manifest, contents: &[&str]) -> Vec<u8> {
    let mut bytes = format!("{header}\n{}\n", serde_json::to_string(manifest).unwrap());
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
    let bytes = export_bytes(HEADER, &manifest, &["abc", "de"]);
    let import = Import::read(&mut &bytes[..]).unwrap();
    let read: Vec<_> = import
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
    let short = export_bytes(HEADER, &manifest, &["abc", "d"]);
    let error = Import::read(&mut &short[..]).err().unwrap();
    assert!(error.to_string().contains("ends inside"), "{error}");
    for path in ["../escape", "/absolute"] {
        manifest.session_files = vec![session_file(path, 1)];
        let bytes = export_bytes(HEADER, &manifest, &["x"]);
        let error = Import::read(&mut &bytes[..]).err().unwrap();
        assert!(error.to_string().contains("outside"), "{error}");
    }
    // A version 1 export has no session files, whatever its manifest says.
    manifest.session_files = vec![session_file("p/s1.jsonl", 1)];
    let bytes = export_bytes(HEADER_WITHOUT_SESSION_FILES, &manifest, &[]);
    assert!(Import::read(&mut &bytes[..]).unwrap().sessions.is_empty());
}

/// An export's manifest with one stopped agent and a last session, both with
/// sessions, made on machine `m1`.
fn manifest_with_sessions() -> Manifest {
    let record: Record = serde_json::from_value(serde_json::json!({
        "agent": "claude",
        "session": "s1",
        "place": "tab",
    }))
    .unwrap();
    let agent = StoppedAgent {
        execution: "execution".into(),
        record,
        stopped_at: 1,
    };
    let last = LastSession {
        execution: "quit".into(),
        agent: "codex".into(),
        session: Some("s2".into()),
        moved: false,
        origin: Some(
            serde_json::from_value(serde_json::json!({"host": "herdr", "place": "tab"})).unwrap(),
        ),
        stopped_by: None,
    };
    let workspace: RemovedWorkspace = serde_json::from_value(serde_json::json!({
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
        "links": {"issue": null, "prs": []},
        "agents": [agent],
        // Earlier removal records kept a single last session.
        "last_session": last,
        "cause": "manual",
        "removed_at": 1,
        "expires_at": 2,
    }))
    .unwrap();
    Manifest {
        remote_url: None,
        prerequisite: None,
        bundle: false,
        sessions: Some(Sessions {
            machine: Some("m1".into()),
            agents: workspace.agents.clone(),
            last_sessions: workspace.last_sessions.clone(),
        }),
        workspace: moved(workspace),
        session_files: Vec::new(),
    }
}

#[test]
fn an_import_keeps_sessions_on_the_exporting_machine_or_when_asked() {
    let mut manifest = manifest_with_sessions();
    let here = manifest.workspace_on(Some("m1"), None);
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
    assert_eq!((last.session.as_deref(), last.moved), (Some("s2"), false));
    assert_eq!(place(last.origin.as_ref()).as_deref(), Some("tab"));

    for new in [
        manifest.workspace_on(Some("m2"), None),
        manifest.workspace_on(None, None),
        manifest.workspace_on(Some("m1"), Some(false)),
    ] {
        assert!(new.agents[0].record.moved);
        assert_eq!(new.agents[0].record.session, None);
        assert!(new.last_sessions[0].moved);
    }

    // Another machine told to keep the sessions keeps no terminal place of
    // the exporting machine's.
    let kept = manifest.workspace_on(Some("m2"), Some(true));
    assert!(!kept.agents[0].record.moved);
    assert_eq!(kept.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(kept.agents[0].record.origin, None);
    assert!(!kept.last_sessions[0].moved);
    assert_eq!(kept.last_sessions[0].origin, None);

    // So does an export from a machine without an ID.
    manifest.sessions.as_mut().unwrap().machine = None;
    assert!(manifest.workspace_on(None, None).agents[0].record.moved);
    let kept = manifest.workspace_on(None, Some(true));
    assert_eq!(kept.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(kept.agents[0].record.origin, None);

    // Another machine keeps a session whose files the export carries.
    manifest.session_files = vec![session_file("p/s1.jsonl", 1)];
    let carried = manifest.workspace_on(Some("m2"), None);
    assert_eq!(carried.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(carried.agents[0].record.origin, None);
    assert!(carried.last_sessions[0].moved);
    let new = manifest.workspace_on(Some("m2"), Some(false));
    assert!(new.agents[0].record.moved);
    manifest.session_files.clear();

    // An agent that never reported a session starts a new one either way.
    let sessions = manifest.sessions.as_mut().unwrap();
    sessions.last_sessions[0].session = None;
    let kept = manifest.workspace_on(None, Some(true));
    assert!(!kept.agents[0].record.moved);
    assert!(kept.last_sessions[0].moved);
}

#[test]
fn moved_records_keep_no_session_place_or_wrapper() {
    let record: Record = serde_json::from_value(serde_json::json!({
        "agent": "codex",
        "session": "s1",
        "place": "tab",
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
    use std::os::unix::fs::PermissionsExt;
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
    let unreadable = source("s2", "unreadable.jsonl");
    fs::set_permissions(&unreadable.source, fs::Permissions::from_mode(0o000)).unwrap();
    let (opened, warnings) = open_sessions(vec![kept, readable, unreadable]);
    let paths: Vec<_> = opened.iter().map(|(file, _)| file.path.clone()).collect();
    assert_eq!(paths, [PathBuf::from("kept.jsonl")]);
    assert_eq!(opened[0].0.size, "kept.jsonl".len() as u64);
    let [warning] = warnings.as_slice() else {
        panic!("one warning: {warnings:?}");
    };
    assert!(warning.contains("claude session s2"), "{warning}");
}
