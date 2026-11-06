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
        .export_space(
            workspace,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
        )
        .await
        .unwrap();
    assert!(exported.removal.is_some());
    // The daemon writes new files only.
    assert!(
        manager
            .export_space(
                workspace,
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
                file.clone(),
            )
            .await
            .is_err()
    );
    fs::read(file).unwrap()
}

async fn import(
    manager: &Manager,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    let packaged = packaged_workspace(&import.manifest);
    import_record(manager, repo, &import, packaged.workspace.clone()).await
}

/// Import the workspace `import` packages as `workspace` says, as `shoal
/// move import` does, which keeps the sessions on the exporting machine.
async fn import_here(
    manager: &Manager,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    let sessions = import
        .manifest
        .sessions_on(crate::process::machine::id().as_deref(), None);
    let workspace = packaged_workspace(&import.manifest).workspace_with(sessions);
    import_record(manager, repo, &import, workspace).await
}

async fn import_record(
    manager: &Manager,
    repo: &Repository,
    import: &Import,
    workspace: RemovedWorkspace,
) -> crate::model::RestoredWorkspace {
    let packaged = packaged_workspace(&import.manifest);
    manager
        .import_workspace(
            &repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            packaged.prerequisite.as_deref(),
            workspace,
            import.manifest.copy.is_some(),
        )
        .await
        .unwrap()
}

/// The workspace `manifest` packages.
fn packaged_workspace(manifest: &Manifest) -> &PackagedWorkspace {
    match &manifest.space {
        Space::Workspace(packaged) => packaged,
        Space::Chat(_) => panic!("a chat package: {manifest:?}"),
    }
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
        &crate::execution::TrackedAgent::named("claude"),
        Some("overload"),
        None,
    )
    .unwrap();

    let file = export_removed(&exporting, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert_eq!(read.manifest.remote_url.as_deref(), origin.to_str());
    assert!(packaged_workspace(&read.manifest).bundle);
    // The exporting machine's import can resume the session it recorded.
    let sessions = read.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.machine, crate::process::machine::id());
    assert_eq!(sessions.agents[0].record.session.as_deref(), Some("s1"));
    let restored = import(&importing, &target_repo, &file).await;

    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let path = &restored.opened.workspace.path;
    assert!(path.starts_with(target_repo.path.parent().unwrap()));
    assert_eq!(restored.opened.workspace.branch.as_deref(), Some("topic"));
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
    let packaged = packaged_workspace(&import.manifest);
    let imported = importing
        .import_workspace(
            &target_repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            packaged.prerequisite.as_deref(),
            packaged.workspace.clone(),
            false,
        )
        .await;
    assert!(imported.is_err());
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
    let packaged = packaged_workspace(&read.manifest);
    assert!(!packaged.bundle);
    assert_eq!(packaged.prerequisite.as_deref(), Some(head.trim()));
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

    let exported = exporting
        .export_space(
            &workspace.id,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            blocked.join("workspace.shoal"),
        )
        .await;
    assert!(exported.is_err());
    assert!(exporting.workspace(&workspace.id).await.is_ok());
    assert!(workspace.path.exists());
}

#[test]
fn import_refuses_other_files_and_newer_formats() {
    assert!(Import::read(&mut &b"not an export\n"[..]).is_err());
    assert!(Import::read(&mut &b"shoal workspace 6\n{}\n"[..]).is_err());
}

/// The header of format version `version`.
fn header(version: u32) -> String {
    format!("{FORMAT} {version}")
}

#[test]
fn a_workspace_package_keeps_the_version_earlier_shoals_read() {
    let mut manifest = manifest_with_sessions();
    assert_eq!(version(&manifest), 1);
    manifest.session_files = vec![session_file("p/s1.jsonl", 1)];
    assert_eq!(version(&manifest), 2);
    // A workspace at a commit needs an importer that opens it without a
    // branch; one before it refuses the package.
    let mut at_commit = manifest.clone();
    if let Space::Workspace(packaged) = &mut at_commit.space {
        packaged.workspace.at = Some(crate::model::StartingCommit {
            commit: "abc".into(),
            made_for: None,
        });
    }
    assert_eq!(version(&at_commit), 4);
    // A copy needs an importer that opens it as a new space; one before it
    // would open the space it copies.
    let copy = Manifest {
        copy: Some(Copied::default()),
        ..manifest.clone()
    };
    assert_eq!(version(&copy), 5);
    // Earlier versions name no kind; their manifests hold a workspace.
    let mut value = serde_json::to_value(&manifest).unwrap();
    assert_eq!(value["kind"], "workspace");
    value.as_object_mut().unwrap().remove("kind");
    let bytes = format!("{}\n{value}\nx", header(2)).into_bytes();
    let import = Import::read(&mut &bytes[..]).unwrap();
    assert_eq!(packaged_workspace(&import.manifest).workspace.name, "topic");
    assert_eq!(import.sessions.len(), 1);
}

#[test]
fn a_chat_package_names_its_kind_and_continues_its_session() {
    let chat = Chat {
        mode: "plan".into(),
        number: 1,
        id: "c1".into(),
        repository_id: "r1".into(),
        name: "plan-cache".into(),
        agent: "claude".into(),
        workspace: None,
        pid: 1,
        started_at: 0,
        origin: None,
        tmp_dir: None,
        session: Some("s1".into()),
        restore: None,
        links: crate::model::SpaceLinks {
            issue: Some("https://forge.example/team/repo/issues/12".into()),
            issue_title: Some("Plan the cache".into()),
            prs: vec!["https://forge.example/team/repo/pulls/14".into()],
            opened: Vec::new(),
            related: vec![crate::model::LinkedItem {
                kind: crate::forge::items::link::LinkKind::Issue,
                url: "https://forge.example/team/repo/issues/9".into(),
                title: None,
            }],
        },
        review: Vec::new(),
        completion: None,
        agent_state: None,
        parent: None,
        children: Vec::new(),
    };
    let mut manifest = Manifest {
        remote_url: None,
        space: Space::Chat(PackagedChat {
            name: chat.name.clone(),
            agent: chat.agent.clone(),
            links: chat.links.clone(),
            mode: chat.mode.clone(),
        }),
        sessions: Some(Sessions {
            machine: Some("m1".into()),
            agents: Vec::new(),
            last_sessions: vec![chat_session(&chat)],
        }),
        session_files: vec![session_file("p/s1.jsonl", 3)],
        copy: None,
    };
    assert_eq!(version(&manifest), 3);
    let bytes = export_bytes(&header(3), &manifest, &["abc"]);
    let import = Import::read(&mut &bytes[..]).unwrap();
    assert!(import.bundle.is_none());
    assert_eq!(import.manifest.name(), "plan-cache");
    let Space::Chat(packaged) = &import.manifest.space else {
        panic!("expected a chat");
    };
    assert_eq!(packaged.agent, "claude");
    assert_eq!(packaged.mode, "plan");
    // Its links come back with their relations, for the import to restore.
    let relations: Vec<_> = (packaged.links.relations().into_iter())
        .map(|(relation, item)| (relation, item.url))
        .collect();
    assert_eq!(
        relations,
        [
            (
                crate::model::Relation::Assigned,
                chat.links.issue.clone().unwrap()
            ),
            (crate::model::Relation::Assigned, chat.links.prs[0].clone()),
            (
                crate::model::Relation::Related,
                chat.links.related[0].url.clone()
            ),
        ]
    );
    // A package an earlier release wrote names no links, and its chat ran
    // in the chat mode, whatever its session's record defaults to.
    let mut value = serde_json::to_value(&manifest).unwrap();
    value.as_object_mut().unwrap().remove("links");
    value.as_object_mut().unwrap().remove("mode");
    let earlier = format!("{}\n{value}\nabc", header(VERSION)).into_bytes();
    let earlier = Import::read(&mut &earlier[..]).unwrap();
    assert!(
        matches!(&earlier.manifest.space, Space::Chat(chat) if chat.links.relations().is_empty() && chat.mode == "chat")
    );
    let reported: Vec<_> = import
        .manifest
        .sessions_on(Some("m2"), None)
        .reported()
        .map(|(agent, session)| (agent.to_owned(), session.to_owned()))
        .collect();
    assert_eq!(reported, [("claude".to_owned(), "s1".to_owned())]);
    // Without its files, another machine starts a new session.
    manifest.session_files.clear();
    assert_eq!(manifest.sessions_on(Some("m2"), None).reported().count(), 0);
    assert_eq!(manifest.sessions_on(Some("m1"), None).reported().count(), 1);
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
    let bytes = export_bytes(&header(2), &manifest, &["abc", "de"]);
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
    let short = export_bytes(&header(2), &manifest, &["abc", "d"]);
    assert!(Import::read(&mut &short[..]).is_err());
    for path in ["../escape", "/absolute"] {
        manifest.session_files = vec![session_file(path, 1)];
        let bytes = export_bytes(&header(2), &manifest, &["x"]);
        assert!(Import::read(&mut &bytes[..]).is_err(), "{path}");
    }
    // A version 1 export has no session files, whatever its manifest says.
    manifest.session_files = vec![session_file("p/s1.jsonl", 1)];
    let bytes = export_bytes(&header(1), &manifest, &[]);
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
        stopped_by: None,
        stop: None,
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
        sessions: Some(Sessions {
            machine: Some("m1".into()),
            agents: workspace.agents.clone(),
            last_sessions: workspace.last_sessions.clone(),
        }),
        space: Space::Workspace(PackagedWorkspace {
            prerequisite: None,
            bundle: false,
            workspace: moved(workspace),
        }),
        session_files: Vec::new(),
        copy: None,
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
    assert_eq!((last.session.as_deref(), last.moved), (Some("s2"), false));
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
    assert_eq!(kept.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(kept.agents[0].record.origin, None);
    assert!(!kept.last_sessions[0].moved);
    assert_eq!(kept.last_sessions[0].origin, None);

    // So does an export from a machine without an ID.
    manifest.sessions.as_mut().unwrap().machine = None;
    assert!(manifest.sessions_on(None, None).agents[0].record.moved);
    let kept = manifest.sessions_on(None, Some(true));
    assert_eq!(kept.agents[0].record.session.as_deref(), Some("s1"));
    assert_eq!(kept.agents[0].record.origin, None);

    // Another machine keeps a session whose files the export carries.
    manifest.session_files = vec![session_file("p/s1.jsonl", 1)];
    let carried = manifest.sessions_on(Some("m2"), None);
    assert_eq!(carried.agents[0].record.session.as_deref(), Some("s1"));
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
    };
    let manifest = Manifest {
        copy: Some(copied),
        ..manifest_with_sessions()
    };
    let here = manifest.sessions_on(Some("m1"), None);
    let agent = &here.agents[0].record;
    assert_eq!(agent.session.as_deref(), Some("s1"));
    assert_eq!((agent.origin.as_ref(), agent.copied), (None, Some(copied)));
    let last = &here.last_sessions[0];
    assert_eq!(last.session.as_deref(), Some("s2"));
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

#[tokio::test]
async fn a_copy_keeps_the_workspace_and_imports_on_a_branch_of_its_own() {
    use crate::{
        forge::items::link::LinkKind,
        model::{LinkedItem, Relation},
    };
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    let item = |kind, url: &str| LinkedItem {
        kind,
        url: url.into(),
        title: None,
    };
    let pr = item(LinkKind::Pr, "https://forge.test/o/r/pulls/2");
    let issue = item(LinkKind::Issue, "https://forge.test/o/r/issues/3");
    manager
        .record_items(
            &workspace.id,
            vec![(Relation::Opened, pr), (Relation::Related, issue.clone())],
        )
        .await
        .unwrap();
    recovery::save_session(&manager.paths, &workspace.id, "stopped-agent", "s1").unwrap();
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        "stopped-agent",
        &crate::execution::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");

    let exported = manager
        .copy_space(&workspace.name, None, file.clone())
        .await
        .unwrap();
    assert!(exported.removal.is_none());
    assert_eq!(exported.manifest.copy, Some(Copied::default()));
    let kept = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(kept.path, workspace.path);
    assert_eq!(
        recovery::Saved::load(&manager.paths, &workspace.id)
            .unwrap()
            .agents
            .len(),
        1
    );
    let bytes = fs::read(&file).unwrap();
    assert!(bytes.starts_with(header(5).as_bytes()));
    let restored = import_here(&manager, &repo, &bytes).await;

    let copy = &restored.opened.workspace;
    assert_ne!(copy.id, workspace.id);
    assert_eq!(copy.branch.as_deref(), Some("topic-2"));
    assert_eq!(git(&copy.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(copy.path.join("scratch")).unwrap(),
        "notes\n"
    );
    let links = manager.workspace(&copy.id).await.unwrap().links;
    assert!(links.opened.is_empty(), "{links:?}");
    assert_eq!(links.related, [issue]);
    // Its agent starts from a fork of the session it reported here, which
    // only a machine with an ID knows it exported; without one, nor any of
    // the session's files, it starts anew.
    let saved = recovery::Saved::load(&manager.paths, &copy.id).unwrap();
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    let forked = crate::process::machine::id().map(|_| "s1");
    assert_eq!(record.session.as_deref(), forked);
    assert_eq!(record.copied, Some(Copied::default()));
    assert!(!record.moved);
    // The original keeps its branch, files and agent.
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), head);
    assert!(workspace.path.join("scratch").exists());
}

#[tokio::test]
async fn a_copy_of_an_earlier_checkpoint_marks_its_sessions_newer() {
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let first = manager.take_checkpoint(&workspace.id, None).await.unwrap();
    fs::write(workspace.path.join("later"), "later\n").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");

    let exported = manager
        .copy_space(&workspace.name, Some(first.number), file.clone())
        .await
        .unwrap();
    assert_eq!(
        exported.manifest.copy,
        Some(Copied {
            earlier_files: true
        })
    );
    let restored = import(&manager, &repo, &fs::read(&file).unwrap()).await;
    assert!(!restored.opened.workspace.path.join("later").exists());
    assert!(
        manager
            .copy_space(
                &workspace.name,
                Some(99),
                directory.path().join("missing.shoal")
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_removed_workspace_has_nothing_to_copy() {
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let refused = manager
        .copy_space("topic", None, directory.path().join("topic.shoal"))
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("was removed"),
        "{refused:#}"
    );
}

#[tokio::test]
async fn concurrent_imports_of_a_copy_take_branches_of_their_own() {
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");
    manager
        .copy_space(&workspace.name, None, file.clone())
        .await
        .unwrap();
    let bytes = fs::read(&file).unwrap();

    let (first, second) = tokio::join!(
        import_here(&manager, &repo, &bytes),
        import_here(&manager, &repo, &bytes)
    );
    let mut branches = [first, second].map(|restored| restored.opened.workspace.branch.unwrap());
    branches.sort();
    assert_eq!(branches, ["topic-2", "topic-3"]);
}
