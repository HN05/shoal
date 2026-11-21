use std::sync::Arc;

use rusqlite::params;
use serde_json::{Value, json};

use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        links::wait::{
            branch_report::BranchReport,
            start::{Start, StartRead},
            test_support::register_watch,
        },
        test_support::{configure, manager, repository},
        workspace_files::Attacher,
    },
    forge::items::link::Selection,
    model::{CleanupReason, Workspace},
};
use crate::{
    forge::{items::link::LinkKind, updates::UpdateKind},
    model::Relation,
};
use shoal_forge::test_support::Server;
use shoal_links::wait::merge_lookups::UNFOLLOWED_MERGE;
use shoal_protocol::{assignments::Done, space_names::SpaceSelector};
use std::time::Duration;

/// The merge commit the forge reports for the merged PR.
const MERGED: &str = "0123456789abcdef0123456789abcdef01234567";

/// A workspace that watches a PR on a forge that reports it merged and
/// lists `runs` for the default branch's merge commit, in a repository
/// with `[pr] watch_merge_runs` set to `watch`.
struct Following {
    root: tempfile::TempDir,
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
        root,
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
                self.manager.links.poll_start(),
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

    /// Start the PR's watch from a read of it as `snapshot`, as `start`
    /// selects, returning the updates it holds.
    async fn start(&self, snapshot: Snapshot, start: Start) -> Vec<crate::forge::updates::Update> {
        let read = StartRead {
            start: self.manager.links.poll_start(),
            reads: vec![(self.pr.clone(), snapshot)],
        };
        let id = &self.workspace.id;
        (self.manager.record_watch_start(id, read, start))
            .await
            .unwrap();
        let urls = [self.pr.clone()];
        self.manager.pending_activity(id, &urls).await.unwrap()
    }

    /// Look up the runs of `commit`, which the workspace's `shoal merge`
    /// pushed to `main` of a local mirror and of the repository at `remote`.
    async fn push(&self, commit: &str, remote: String) {
        let pushed = PushedMerge {
            space: self.workspace.id.clone(),
            remotes: vec!["/srv/mirror.git".into(), remote],
            branch: "main".into(),
            commit: commit.into(),
        };
        self.manager.follow_pushed_merge(pushed).await.unwrap();
    }

    /// The forge's repository, as a remote URL.
    fn forge_remote(&self) -> String {
        format!("http://{}/team/repo.git", self.server.host)
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
            .run(move |db| Ok(shoal_links::linked::space_links(db, &id)?))
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
async fn a_restart_that_reads_the_merge_follows_it() {
    let following = following(true, json!([listed_run("running")])).await;
    let pr = following.pr.clone();
    following.observe(&pr, pull("open", None)).await;
    let merged = following.start(pull("merged", Some(MERGED)), Start::Every);
    let merged = merged.await;
    assert_eq!(merged[0].kind, UpdateKind::Merged, "{merged:?}");
    assert!(following.awaits().await);
    // The watch holds the merge, so PR cleanup counts it seen.
    let unseen = following.manager.merge_unseen(&following.workspace.id);
    assert!(!unseen.await.unwrap());
}

#[tokio::test]
async fn linking_a_merged_pr_follows_its_merge() {
    let following = following(true, json!([listed_run("running")])).await;
    let merged = following.start(pull("merged", Some(MERGED)), Start::Unread);
    let merged = merged.await;
    assert_eq!(merged[0].kind, UpdateKind::Merged, "{merged:?}");
    assert!(following.awaits().await);
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

#[tokio::test]
async fn pr_cleanup_waits_for_a_poll_to_see_the_merge() {
    let following = following(true, json!([])).await;
    let (manager, workspace) = (&following.manager, &following.workspace);
    let done = manager
        .mark_space_done(
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
    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        (manager)
            .record_background_activity(&workspace.id, vec![(following.pr.clone(), snapshot)])
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
async fn a_merge_seen_while_its_default_branch_is_unknown_is_followed_by_the_next_read() {
    let following = following(true, json!([])).await;
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    let record = |snapshot| {
        let observations = vec![(pr.clone(), snapshot)];
        (following.manager).record_background_activity(&id, observations)
    };
    record(pull("open", None)).await.unwrap();
    // Without a remote, the checkout's branch names the default branch.
    let checkout = following.root.path().join("repo");
    crate::daemon::test_support::git(&checkout, &["checkout", "-q", "--detach"]);
    assert!(record(pull("merged", Some(MERGED))).await.is_err());
    assert!(!following.awaits().await);

    crate::daemon::test_support::git(&checkout, &["checkout", "-q", "main"]);
    record(pull("merged", Some(MERGED))).await.unwrap();
    assert!(following.awaits().await);
}

#[tokio::test]
async fn a_run_the_background_poll_saw_finish_ends_its_wait_while_its_updates_wait() {
    let following = following(true, json!([listed_run("running")])).await;
    following.merge(Some(MERGED)).await;
    following.follow().await;
    let (run, id) = (run_url(&following), following.workspace.id.clone());
    let read = |state: &str, errors: Value| -> Snapshot {
        serde_json::from_value(json!({"comments": {}, "checks": {}, "conflict": null,
            "state": state, "errors": errors}))
        .unwrap()
    };
    // A failed read waits on the cursor for an agent that does not take it.
    let failed = following
        .observe(&run, read("running", json!({"CI run": "unreachable"})))
        .await;
    assert_eq!(failed[0].kind, UpdateKind::LookupFailed, "{failed:?}");
    following.end_window().await;
    following.follow().await;
    assert!(following.awaits().await);

    // The background poll's own reads see it finish.
    for state in ["running", "failed"] {
        (following.manager)
            .record_background_activity(&id, vec![(run.clone(), read(state, json!({})))])
            .await
            .unwrap();
    }
    assert!(!following.awaits().await);
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
        .mark_space_done(
            &SpaceSelector::id(&chat.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let present = || async {
        let chats = manager.chats().await.unwrap();
        chats.iter().any(|listed| listed.id == chat.id)
    };
    manager.sweep_completed().await.unwrap();
    assert!(present().await, "the PR's merge is unseen");

    // The poll that sees the merge looks up its runs until the window ends.
    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        (manager)
            .record_background_activity(&chat.id, vec![(following.pr.clone(), snapshot)])
            .await
            .unwrap();
    }
    let space = LinkSpace::own(manager.space_by_id(&chat.id).await.unwrap()).unwrap();
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
    manager.follow_merge_runs(&space).await.unwrap();
    manager.sweep_completed().await.unwrap();
    assert!(!present().await);
}

#[tokio::test]
async fn a_cursor_that_sees_the_merge_after_its_lookup_ended_starts_none() {
    let following = following(true, json!([])).await;
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    hold_a_comment(&following).await;
    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        (following.manager)
            .record_background_activity(&id, vec![(pr.clone(), snapshot)])
            .await
            .unwrap();
    }
    following.end_window().await;
    following.follow().await;
    assert!(!following.awaits().await);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 1);

    // The agent takes the held comment, and the cursor then sees the merge.
    let held = following.observe(&pr, pull("merged", Some(MERGED))).await;
    let deliveries = held.into_iter().map(|update| update.delivery).collect();
    (following.manager)
        .acknowledge_pr_updates(&SpaceSelector::id(&id), deliveries)
        .await
        .unwrap();
    let merged = following.observe(&pr, pull("merged", Some(MERGED))).await;
    let kinds: Vec<_> = merged.iter().map(|update| update.kind).collect();
    assert!(kinds.contains(&UpdateKind::Merged), "{merged:?}");
    assert!(!following.awaits().await);
    following.follow().await;
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 1);
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
        (self.manager.poll_space_item_activity(&space))
            .await
            .unwrap();
    }

    /// How many reads of the PR the forge received.
    fn pr_reads(&self) -> usize {
        let read = "GET /api/v1/repos/team/repo/pulls/1";
        let received = self.server.received().into_iter();
        received.filter(|request| request.line == read).count()
    }
}

#[tokio::test]
async fn the_background_poll_follows_the_merge_of_a_pr_linked_as_opened() {
    let following = opening(true, json!([listed_run("running")])).await;
    following.poll().await;
    assert!(following.awaits().await);
    following.poll().await;
    assert_eq!(following.related().await, [run_url(&following)]);
    // Once seen merged, the PR is read no longer.
    let reads = following.pr_reads();
    following.poll().await;
    assert_eq!(following.pr_reads(), reads);
}

#[tokio::test]
async fn the_merge_of_a_pr_linked_as_opened_reports_nothing_of_the_pr() {
    let following = opening(true, json!([])).await;
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        (following.manager)
            .record_background_activity(&id, vec![(pr.clone(), snapshot)])
            .await
            .unwrap();
    }
    assert!(following.awaits().await);
    assert_eq!(following.events(EventKind::ItemActivity).await, 0);
    following.poll().await;
    let pending = following.manager.pending_activity(&id, &[pr]).await;
    assert!(pending.unwrap().is_empty());
}

#[tokio::test]
async fn a_pr_linked_as_opened_is_not_read_where_the_option_is_off() {
    let following = opening(false, json!([listed_run("running")])).await;
    following.poll().await;
    assert_eq!(following.pr_reads(), 0);
    assert!(!following.awaits().await);
}

#[tokio::test]
async fn completion_cleanup_waits_for_a_poll_to_see_an_opened_prs_merge() {
    let following = opening(true, json!([])).await;
    let (manager, workspace) = (&following.manager, &following.workspace);
    manager
        .mark_space_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert!(workspace.path.exists(), "the PR's merge is unseen");

    following.poll().await;
    manager.sweep_completed().await.unwrap();
    assert!(workspace.path.exists(), "the merge's runs are looked up");

    following.end_window().await;
    following.follow().await;
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_pushed_merge_links_the_default_branchs_runs_for_its_commit() {
    let following = following(true, json!([listed_run("running")])).await;
    following.push(MERGED, following.forge_remote()).await;
    assert!(following.awaits().await);

    following.follow().await;
    let listings = following.listings();
    assert_eq!(listings.len(), 1, "{listings:?}");
    assert!(
        listings[0].contains("ref=refs%2Fheads%2Fmain"),
        "{listings:?}"
    );
    assert!(listings[0].contains(&format!("head_sha={MERGED}")));
    assert_eq!(following.related().await, [run_url(&following)]);
}

#[tokio::test]
async fn a_pushed_merge_is_not_followed_where_the_option_is_off() {
    let following = following(false, json!([listed_run("running")])).await;
    following.push(MERGED, following.forge_remote()).await;
    assert!(!following.awaits().await);
}

#[tokio::test]
async fn a_pushed_merge_whose_forge_cannot_be_found_ends_with_its_window() {
    let following = following(true, json!([listed_run("running")])).await;
    // The host answers nothing that says which forge it serves.
    let host = Server::start(|_| (404, Vec::new(), Vec::new()));
    let remote = format!("http://{}/team/repo.git", host.host);
    following.push(MERGED, remote).await;
    let space = LinkSpace::from(following.workspace.clone());
    assert!(following.manager.follow_merge_runs(&space).await.is_err());
    assert!(following.awaits().await);

    following.end_window().await;
    following.follow().await;
    assert!(!following.awaits().await);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 1);
}
