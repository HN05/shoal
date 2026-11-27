use std::sync::Arc;

use rusqlite::params;
use serde_json::{Value, json};

use crate::forge::updates::SnapshotState;
use crate::{
    daemon::{
        links::wait::test_support::register_watch,
        manager::Manager,
        test_support::{configure, manager, repository},
        workspace_files::Attacher,
    },
    model::{CleanupReason, Workspace},
    time::UnixTime,
};
use crate::{forge::items::link::LinkKind, model::Relation};
use shoal_forge::test_support::Server;
use shoal_forge::updates::Snapshot;
use shoal_links::{space::LinkSpace, wait::merge_lookups::LOOKUP_WINDOW};
use shoal_protocol::{assignments::Done, space_names::SpaceSelector};
use std::time::Duration;

/// The merge commit the forge reports for the merged PR.
const MERGED: &str = "0123456789abcdef0123456789abcdef01234567";

/// A workspace that watches a PR on a forge that reports it merged and
/// lists `runs` for the default branch's merge commit, in a repository
/// with `[pr] watch_merge_runs` set to `watch`.
struct Following {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
    _server: Server,
    pr: String,
}

async fn following(watch: bool, runs: Value) -> Following {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let server = Server::start(move |request| {
        if (request.line).starts_with("GET /api/v1/repos/team/repo/actions/runs?") {
            let listed = json!({"workflow_runs": runs});
            return (200, Vec::new(), listed.to_string().into());
        }
        // The watched PR has merged on the forge.
        if request.line == "GET /api/v1/repos/team/repo/pulls/1" {
            let pull = json!({"number": 1, "title": "Fix", "state": "closed", "merged": true,
                "merge_commit_sha": MERGED, "head": {"ref": "topic", "sha": "head"},
                "base": {"ref": "main"}});
            return (200, Vec::new(), pull.to_string().into());
        }
        (404, Vec::new(), Vec::new())
    });
    let host = &server.host;
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n\n\
             [cleanup]\ngrace_minutes = 0\n\n[pr]\nwatch_merge_runs = {watch}\n"
        ),
    )
    .await;
    let workspace = manager
        .create_workspace(&repo.id, "merging".into(), None, None, None)
        .await
        .unwrap();
    let pr = format!("http://{host}/team/repo/pulls/1");
    register_watch(&manager, &workspace.id, &pr).await;
    Following {
        _root: root,
        manager,
        workspace,
        _server: server,
        pr,
    }
}

/// A read of the PR into `main` in `state`, with the merge commit
/// `merge_commit` when it has one.
fn pull(state: SnapshotState, merge_commit: Option<&str>) -> Snapshot {
    let mut record = json!({"comments": {}, "checks": {}, "conflict": null, "errors": {},
        "details": {"title": "Fix", "body": "", "labels": [], "assignees": [], "base": "main"}});
    if let Some(commit) = merge_commit {
        record["merge_commit"] = commit.into();
    }
    Snapshot::stored(Some(state), &record.to_string()).unwrap()
}

impl Following {
    async fn follow(&self) {
        let space = LinkSpace::from(self.workspace.clone());
        self.manager.links.follow_merge_runs(&space).await.unwrap();
    }

    /// Move the lookup's start back past its window.
    async fn end_window(&self) {
        let id = self.workspace.id.clone();
        let started = UnixTime::now() - LOOKUP_WINDOW - Duration::from_secs(60);
        (self.manager.store)
            .run(move |db| {
                db.execute(
                    "UPDATE merge_runs SET merged_at=?2 WHERE space_id=?1",
                    params![id, started],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn completion_cleanup_waits_for_the_runs_of_a_merge() {
    let following = following(true, json!([])).await;
    let (manager, workspace) = (&following.manager, &following.workspace);
    // Without a PR watch, completion cleanup alone decides the removal.
    let (id, pr) = (workspace.id.clone(), following.pr.clone());
    (manager.store)
        .run(move |db| {
            db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [&id])?;
            db.execute(
                "INSERT INTO merge_runs(space_id,pr_url,merge_commit,branch,merged_at)
                 VALUES (?1,?2,?3,'main',?4)",
                params![id, pr, MERGED, UnixTime::now()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(workspace.path.exists());
    let space = manager.spaces.space_by_id(&workspace.id).await.unwrap();
    assert_eq!(
        manager.spaces.cleanup_reason(&**manager, &space).await,
        CleanupReason::MergeRuns
    );

    following.end_window().await;
    following.follow().await;
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn pr_cleanup_waits_for_a_poll_to_see_the_merge() {
    let following = following(true, json!([])).await;
    let (manager, workspace) = (&following.manager, &following.workspace);
    let done = manager
        .spaces
        .mark_space_done(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The PR sweep has already found every watched PR merged at HEAD.
    let registration = crate::daemon::links::pr::Registration {
        kind: crate::daemon::links::pr::RegistrationKind::Watch {
            urls: vec![following.pr.parse().unwrap()],
            merged_head: Some(done.head),
        },
        error: None,
    };
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute(
                "UPDATE pr_cleanup SET record=?2 WHERE space_id=?1",
                params![id, serde_json::to_string(&registration)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());

    // The poll that sees the merge starts its lookup, which keeps the space.
    for snapshot in [
        pull(SnapshotState::Open, None),
        pull(SnapshotState::Merged, Some(MERGED)),
    ] {
        (manager.links)
            .record_background_activity(
                &**manager,
                &workspace.id,
                vec![(following.pr.clone(), snapshot)],
            )
            .await
            .unwrap();
    }
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());

    following.end_window().await;
    following.follow().await;
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn completion_cleanup_waits_for_a_poll_to_see_a_done_chats_merge() {
    let following = following(true, json!([])).await;
    let manager = &following.manager;
    let repository = following.workspace.repository().clone();
    let chat = manager
        .start_chat(repository.into(), "merging".into(), "claude".into(), None)
        .await
        .unwrap();
    register_watch(manager, &chat.id, &following.pr).await;
    let id = chat.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE space_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &**manager,
            &SpaceSelector::id(&chat.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let present = || async {
        let chats = manager.spaces.chats().await.unwrap();
        chats.iter().any(|listed| listed.id == chat.id)
    };
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(present().await, "the PR's merge is unseen");

    // The poll that sees the merge looks up its runs until the window ends.
    for snapshot in [
        pull(SnapshotState::Open, None),
        pull(SnapshotState::Merged, Some(MERGED)),
    ] {
        (manager.links)
            .record_background_activity(
                &**manager,
                &chat.id,
                vec![(following.pr.clone(), snapshot)],
            )
            .await
            .unwrap();
    }
    let space = LinkSpace::own(manager.spaces.space_by_id(&chat.id).await.unwrap()).unwrap();
    let id = chat.id.clone();
    let started = UnixTime::now() - LOOKUP_WINDOW - Duration::from_secs(60);
    (manager.store)
        .run(move |db| {
            db.execute(
                "UPDATE merge_runs SET merged_at=?2 WHERE space_id=?1",
                params![id, started],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.links.follow_merge_runs(&space).await.unwrap();
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(!present().await);
}

/// The fixture of [`following`], whose workspace links its PR as opened
/// instead of watching it.
async fn opening(watch: bool, runs: Value) -> Following {
    let following = following(watch, runs).await;
    let (id, pr) = (following.workspace.id.clone(), following.pr.clone());
    let links = following.manager.links.clone();
    (following.manager.store)
        .run(move |db| {
            db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [&id])?;
            links.record(db, &id, Relation::Opened, LinkKind::Pr, &pr, Some("Fix"))
        })
        .await
        .unwrap();
    following
}

impl Following {
    /// Run the background poll of the workspace once.
    async fn poll(&self) {
        let space = LinkSpace::from(self.workspace.clone());
        (self
            .manager
            .links
            .poll_space_item_activity(&*self.manager, &space))
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn completion_cleanup_waits_for_a_poll_to_see_an_opened_prs_merge() {
    let following = opening(true, json!([])).await;
    let (manager, workspace) = (&following.manager, &following.workspace);
    manager
        .spaces
        .mark_space_done(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(workspace.path.exists(), "the PR's merge is unseen");

    following.poll().await;
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(workspace.path.exists(), "the merge's runs are looked up");

    following.end_window().await;
    following.follow().await;
    manager.spaces.sweep_completed(&**manager).await.unwrap();
    assert!(!workspace.path.exists());
}
