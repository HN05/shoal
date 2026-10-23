use std::{fs, path::Path, sync::Arc};

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
    let error = Import::read(&mut &b"shoal workspace 2\n{}\n"[..])
        .err()
        .unwrap();
    assert!(error.to_string().contains("newer format"), "{error}");
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
        agent: "codex".into(),
        session: Some("s2".into()),
        moved: false,
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
            last_session: workspace.last_session.clone(),
        }),
        workspace: moved(workspace),
    }
}

#[test]
fn an_import_keeps_sessions_on_the_exporting_machine_or_when_asked() {
    let mut manifest = manifest_with_sessions();
    let here = manifest.workspace_on(Some("m1"), None);
    assert_eq!(here.agents, manifest.sessions.as_ref().unwrap().agents);
    assert!(!here.agents[0].record.moved);
    assert_eq!(here.agents[0].record.place.as_deref(), Some("tab"));
    let last = here.last_session.unwrap();
    assert_eq!((last.session.as_deref(), last.moved), (Some("s2"), false));

    for new in [
        manifest.workspace_on(Some("m2"), None),
        manifest.workspace_on(None, None),
        manifest.workspace_on(Some("m1"), Some(false)),
    ] {
        assert!(new.agents[0].record.moved);
        assert_eq!(new.agents[0].record.session, None);
        assert!(new.last_session.unwrap().moved);
    }

    // Another machine told to keep the sessions keeps no terminal place of
    // the exporting machine's.
    let kept = manifest.workspace_on(Some("m2"), Some(true));
    assert!(!kept.agents[0].record.moved);
    assert_eq!(kept.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(kept.agents[0].record.place, None);
    assert!(!kept.last_session.unwrap().moved);

    // So does an export from a machine without an ID.
    manifest.sessions.as_mut().unwrap().machine = None;
    assert!(manifest.workspace_on(None, None).agents[0].record.moved);
    let kept = manifest.workspace_on(None, Some(true));
    assert_eq!(kept.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(kept.agents[0].record.place, None);

    // An agent that never reported a session starts a new one either way.
    let sessions = manifest.sessions.as_mut().unwrap();
    sessions.last_session.as_mut().unwrap().session = None;
    let kept = manifest.workspace_on(None, Some(true));
    assert!(!kept.agents[0].record.moved);
    assert!(kept.last_session.unwrap().moved);
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
        (moved.session, moved.place, moved.waiting),
        (None, None, None)
    );
}
