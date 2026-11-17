use std::sync::Arc;

use rusqlite::params;
use serde_json::{Value, json};

use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        links::wait::{branch_report::BranchReport, tests::register_watch},
        test_support::{configure, manager, repository},
        workspace_files::Attacher,
    },
    forge::items::link::Selection,
    model::{CleanupReason, Workspace},
};
use shoal_forge::test_support::Server;
use shoal_protocol::{assignments::Done, space_names::SpaceSelector};

/// The merge commit the forge reports for the merged PR.
const MERGED: &str = "0123456789abcdef0123456789abcdef01234567";

/// A workspace that watches a PR on a forge listing `runs` for the default
/// branch's merge commit, in a repository with `[pr] watch_merge_runs` set
/// to `watch`.
struct Following {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
    server: Server,
    pr: String,
}

async fn following(watch: bool, runs: Value) -> Following {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let server = Server::start(move |request| {
        if (request.line).starts_with("GET /api/v1/repos/team/repo/actions/runs?") {
            let listed = json!({"workflow_runs": runs});
            return (200, Vec::new(), listed.to_string().into());
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
        server,
        pr,
    }
}

/// A run of the merge commit on `main` in `status`, as Forgejo lists it.
fn listed_run(status: &str) -> Value {
    json!({"id": 41, "index_in_repo": 7, "prettyref": "main", "workflow_id": "ci.yml",
        "head_sha": MERGED, "status": status})
}

/// A read of the PR into `main` in `state`, with the merge commit
/// `merge_commit` when it has one.
fn pull(state: &str, merge_commit: Option<&str>) -> Snapshot {
    let mut snapshot = json!({"comments": {}, "checks": {}, "conflict": null,
        "state": state, "errors": {}, "details": {"title": "Fix", "body": "",
        "labels": [], "assignees": [], "base": "main"}});
    if let Some(commit) = merge_commit {
        snapshot["merge_commit"] = commit.into();
    }
    serde_json::from_value(snapshot).unwrap()
}

impl Following {
    /// Record a watch's read of `url` as `snapshot`, returning its updates.
    async fn observe(&self, url: &str, snapshot: Snapshot) -> Vec<crate::forge::updates::Update> {
        let observations = vec![(url.to_owned(), snapshot)];
        (self.manager)
            .record_observed(
                &self.workspace.id,
                observations,
                &Selection::default(),
                BranchReport::Conflicts,
                UnixTime::now(),
            )
            .await
            .unwrap()
    }

    /// Watch the PR open and then merge with `merge_commit`, returning the
    /// merge's updates.
    async fn merge(&self, merge_commit: Option<&str>) -> Vec<crate::forge::updates::Update> {
        let pr = self.pr.clone();
        self.observe(&pr, pull("open", None)).await;
        let delivered = self.observe(&pr, pull("open", None)).await;
        assert!(delivered.is_empty(), "{delivered:?}");
        self.observe(&pr, pull("merged", merge_commit)).await
    }

    async fn follow(&self) {
        let space = LinkSpace::from(self.workspace.clone());
        self.manager.follow_merge_runs(&space).await.unwrap();
    }

    async fn awaits(&self) -> bool {
        (self.manager.awaits_merge_runs(&self.workspace.id))
            .await
            .unwrap()
    }

    /// The run lists the forge received.
    fn listings(&self) -> Vec<String> {
        (self.server.received().into_iter())
            .map(|request| request.line)
            .filter(|line| line.contains("/actions/runs?"))
            .collect()
    }

    /// The URLs of the items the workspace links as related.
    async fn related(&self) -> Vec<String> {
        let id = self.workspace.id.clone();
        let links = (self.manager.store)
            .run(move |db| Ok(crate::daemon::store::space_links(db, &id)?))
            .await
            .unwrap();
        links
            .related
            .iter()
            .map(|item| item.url.to_string())
            .collect()
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

    async fn events(&self, kind: EventKind) -> usize {
        let events = self.manager.space_events(None, 100).await.unwrap();
        (events.iter())
            .filter(|item| matches!(item, EventItem::Event(event) if event.details.kind == kind))
            .count()
    }
}

/// The URL of the listed run on the forge.
fn run_url(following: &Following) -> String {
    format!("http://{}/team/repo/actions/runs/7", following.server.host)
}

#[tokio::test]
async fn a_merge_links_the_default_branchs_runs_for_its_merge_commit() {
    let following = following(true, json!([listed_run("running")])).await;
    let merged = following.merge(Some(MERGED)).await;
    assert_eq!(merged[0].kind, UpdateKind::Merged, "{merged:?}");
    assert!(!merged[0].message.contains(UNFOLLOWED_MERGE));
    assert!(following.awaits().await);

    following.follow().await;
    let listings = following.listings();
    assert_eq!(listings.len(), 1, "{listings:?}");
    assert!(
        listings[0].contains("ref=refs%2Fheads%2Fmain"),
        "{listings:?}"
    );
    assert!(listings[0].contains(&format!("head_sha={MERGED}")));
    let run = run_url(&following);
    assert_eq!(following.related().await, std::slice::from_ref(&run));

    // Listing it again links it once; it keeps the lookup after the window
    // until a watch sees it finish.
    following.end_window().await;
    following.follow().await;
    assert_eq!(following.related().await, std::slice::from_ref(&run));
    assert!(following.awaits().await);
    let finished = serde_json::from_value(json!({"comments": {}, "checks": {},
        "conflict": null, "state": "failed", "errors": {}}))
    .unwrap();
    let reported = following.observe(&run, finished).await;
    assert_eq!(reported[0].kind, UpdateKind::CiCompleted, "{reported:?}");
    assert!(!following.awaits().await);
    following.follow().await;
    assert_eq!(following.listings().len(), 2);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 0);
}

#[tokio::test]
async fn a_merge_without_a_reported_merge_commit_says_its_runs_are_not_followed() {
    let following = following(true, json!([listed_run("running")])).await;
    let merged = following.merge(None).await;
    assert_eq!(merged[0].kind, UpdateKind::Merged, "{merged:?}");
    assert!(merged[0].message.ends_with(UNFOLLOWED_MERGE), "{merged:?}");
    assert!(!following.awaits().await);
    following.follow().await;
    assert!(following.listings().is_empty());
    assert!(following.related().await.is_empty());
}

#[tokio::test]
async fn a_merge_is_not_followed_where_the_option_is_off() {
    let following = following(false, json!([listed_run("running")])).await;
    let merged = following.merge(Some(MERGED)).await;
    assert!(!merged[0].message.contains(UNFOLLOWED_MERGE), "{merged:?}");
    assert!(!following.awaits().await);
    following.follow().await;
    assert!(following.listings().is_empty());
    assert!(following.related().await.is_empty());
}

#[tokio::test]
async fn a_lookup_that_finds_no_run_in_its_window_ends_in_the_log() {
    let following = following(true, json!([])).await;
    following.merge(Some(MERGED)).await;
    following.follow().await;
    assert!(following.awaits().await);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 0);

    following.end_window().await;
    // The window's last listing still looks once more.
    following.follow().await;
    assert_eq!(following.listings().len(), 2);
    assert!(!following.awaits().await);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 1);
    following.follow().await;
    assert_eq!(following.listings().len(), 2);
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
        .mark_space_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert!(workspace.path.exists());
    let space = manager.space_by_id(&workspace.id).await.unwrap();
    assert_eq!(
        manager.cleanup_reason(&space).await,
        CleanupReason::MergeRuns
    );

    following.end_window().await;
    following.follow().await;
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
}

/// Hold a comment on the PR's cursor for an agent that does not take it,
/// so later reads of the PR replay it.
async fn hold_a_comment(following: &Following) {
    let pr = following.pr.clone();
    following.observe(&pr, pull("open", None)).await;
    let mut commented = serde_json::to_value(pull("open", None)).unwrap();
    commented["comments"] = json!({"discussion:1": {"fingerprint": "first"}});
    let waiting = (following.observe(&pr, serde_json::from_value(commented).unwrap())).await;
    assert_eq!(waiting[0].kind, UpdateKind::Comment, "{waiting:?}");
    let replayed = following.observe(&pr, pull("merged", Some(MERGED))).await;
    assert_eq!(replayed, waiting);
}

#[tokio::test]
async fn a_first_background_read_of_a_merged_pr_follows_its_merge() {
    let following = following(true, json!([])).await;
    hold_a_comment(&following).await;
    assert!(!following.awaits().await);
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    (following.manager)
        .record_background_activity(&id, vec![(pr, pull("merged", Some(MERGED)))])
        .await
        .unwrap();
    assert!(following.awaits().await);
}

#[tokio::test]
async fn the_background_poll_follows_a_merge_while_older_updates_wait() {
    let following = following(true, json!([])).await;
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    hold_a_comment(&following).await;
    assert!(!following.awaits().await);

    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        (following.manager)
            .record_background_activity(&id, vec![(pr.clone(), snapshot)])
            .await
            .unwrap();
    }
    assert!(following.awaits().await);
}
