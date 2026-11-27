use std::{sync::Arc, time::Duration};

use rusqlite::params;
use serde_json::{Value, json};
use shoal_core::test_support::{git, global_config, repository};
use shoal_forge::test_support::Server;
use shoal_forge::updates::Snapshot;
use shoal_forge_types::{
    backends::ForgeKind,
    items::link::{LinkKind, Selection},
    updates::{Update, UpdateKind},
};
use shoal_protocol::{
    events::EventKind,
    links::pr::{Registration, RegistrationKind},
    model::{Relation, Workspace},
    space_names::SpaceSelector,
    test_support,
    time::UnixTime,
};
use shoal_store::Store;

use crate::{
    Links,
    space::LinkSpace,
    stand_in::{StandIn, record, service},
    wait::{
        branch_report::BranchReport,
        merge_lookups::{LOOKUP_WINDOW, PushedMerge, UNFOLLOWED_MERGE},
        start::{Start, StartRead},
    },
};

/// The merge commit the forge reports for the merged PR.
const MERGED: &str = "0123456789abcdef0123456789abcdef01234567";

/// A workspace that watches a PR on a forge that reports it merged and
/// lists `runs` for the default branch's merge commit, in a repository
/// with `[pr] watch_merge_runs` set to `watch`, and the links' service over
/// a stand-in daemon with both.
struct Following {
    root: tempfile::TempDir,
    store: Store,
    links: Arc<Links>,
    daemon: StandIn,
    workspace: Workspace,
    server: Server,
    pr: String,
}

async fn following(watch: bool, runs: Value) -> Following {
    let root = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
        .await
        .unwrap();
    let path = repository(root.path(), "repo");
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
    shoal_forge_types::hosts::learn(host, ForgeKind::Forgejo);
    let config = format!(
        "[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n\n\
         [pr]\nwatch_merge_runs = {watch}\n"
    );
    let workspace = shoal_protocol::test_support::workspace("workspace", "repo", "merging", &path);
    record(&store, &workspace).await;
    let links = service(root.path(), store.clone(), &config);
    let daemon = StandIn {
        workspaces: vec![workspace.clone()],
        repositories: vec![test_support::repository("repo", &path)],
        config: Some(global_config(&config)),
        ..StandIn::default()
    };
    let pr = format!("http://{host}/team/repo/pulls/1");
    let following = Following {
        root,
        store,
        links,
        daemon,
        workspace,
        server,
        pr,
    };
    following.watch().await;
    following
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
    /// Have the workspace watch the PR, as its PR cleanup registration does.
    async fn watch(&self) {
        let (id, pr) = (self.workspace.id.clone(), self.pr.clone());
        let registration = Registration {
            kind: RegistrationKind::Watch {
                urls: vec![pr.parse().unwrap()],
                merged_head: None,
            },
            error: None,
        };
        let watched = self.store.run(move |db| {
            db.execute(
                "INSERT OR REPLACE INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![id, serde_json::to_string(&registration)?],
            )?;
            Ok(())
        });
        watched.await.unwrap();
    }

    /// Record a watch's read of `url` as `snapshot`, returning its updates.
    async fn observe(&self, url: &str, snapshot: Snapshot) -> Vec<Update> {
        let observations = vec![(url.to_owned(), snapshot)];
        let (id, all, report) = (
            &self.workspace.id,
            Selection::default(),
            BranchReport::Conflicts,
        );
        let start = self.links.poll_start();
        let recorded =
            (self.links).record_observed(&self.daemon, id, observations, &all, report, start);
        recorded.await.unwrap()
    }

    /// Record the background poll's read of `url` as `snapshot`.
    async fn record_background(&self, url: &str, snapshot: Snapshot) -> anyhow::Result<()> {
        let observations = vec![(url.to_owned(), snapshot)];
        let id = &self.workspace.id;
        (self.links)
            .record_background_activity(&self.daemon, id, observations)
            .await
            .map(drop)
    }

    /// Start the PR's watch from a read of it as `snapshot`, as `start`
    /// selects, returning the updates it holds.
    async fn start(&self, snapshot: Snapshot, start: Start) -> Vec<Update> {
        let read = StartRead {
            start: self.links.poll_start(),
            reads: vec![(self.pr.clone(), snapshot)],
        };
        let id = &self.workspace.id;
        let started = self.links.record_watch_start(&self.daemon, id, read, start);
        started.await.unwrap();
        let urls = [self.pr.clone()];
        self.links.pending_activity(id, &urls).await.unwrap()
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
        let followed = self.links.follow_pushed_merge(&self.daemon, pushed);
        followed.await.unwrap();
    }

    async fn follow(&self) {
        let space = LinkSpace::from(self.workspace.clone());
        self.links.follow_merge_runs(&space).await.unwrap();
    }

    async fn awaits(&self) -> bool {
        self.links
            .awaits_merge_runs(&self.workspace.id)
            .await
            .unwrap()
    }

    /// Move the lookup's start back past its window.
    async fn end_window(&self) {
        let id = self.workspace.id.clone();
        let started = UnixTime::now() - LOOKUP_WINDOW - Duration::from_secs(60);
        let moved = self.store.run(move |db| {
            db.execute(
                "UPDATE merge_runs SET merged_at=?2 WHERE space_id=?1",
                params![id, started],
            )?;
            Ok(())
        });
        moved.await.unwrap();
    }

    /// How many events of `kind` the workspace's log holds.
    async fn events(&self, kind: EventKind) -> usize {
        let id = self.workspace.id.clone();
        let counted = self.store.run(move |db| {
            let count: i64 = db.query_row(
                "SELECT count(*) FROM space_events
                 WHERE space_id=?1 AND json_extract(record,'$.kind')=?2",
                params![id, kind],
                |row| row.get(0),
            )?;
            Ok(count)
        });
        counted.await.unwrap() as usize
    }

    /// Watch the PR open and then merge with `merge_commit`, returning the
    /// merge's updates.
    async fn merge(&self, merge_commit: Option<&str>) -> Vec<Update> {
        let pr = self.pr.clone();
        self.observe(&pr, pull("open", None)).await;
        let delivered = self.observe(&pr, pull("open", None)).await;
        assert!(delivered.is_empty(), "{delivered:?}");
        self.observe(&pr, pull("merged", merge_commit)).await
    }

    /// The forge's repository, as a remote URL.
    fn forge_remote(&self) -> String {
        format!("http://{}/team/repo.git", self.server.host)
    }

    /// The run lists the forge received.
    fn listings(&self) -> Vec<String> {
        (self.server.received().into_iter())
            .map(|request| request.line)
            .filter(|line| line.contains("/actions/runs?"))
            .collect()
    }

    /// How many reads of the PR the forge received.
    fn pr_reads(&self) -> usize {
        let read = "GET /api/v1/repos/team/repo/pulls/1";
        let received = self.server.received().into_iter();
        received.filter(|request| request.line == read).count()
    }

    /// The URLs of the items the workspace links as related.
    async fn related(&self) -> Vec<String> {
        let id = self.workspace.id.clone();
        let links = self
            .store
            .run(move |db| Ok(crate::linked::space_links(db, &id)?));
        let links = links.await.unwrap();
        (links.related.iter())
            .map(|item| item.url.to_string())
            .collect()
    }

    /// Run the background poll of the workspace once.
    async fn poll(&self) {
        let space = LinkSpace::from(self.workspace.clone());
        let polled = self.links.poll_space_item_activity(&self.daemon, &space);
        polled.await.unwrap();
    }
}

/// The URL of the listed run on the forge.
fn run_url(following: &Following) -> String {
    format!("http://{}/team/repo/actions/runs/7", following.server.host)
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
    let unseen = (following.links).merge_unseen(&following.daemon, &following.workspace.id);
    assert!(!unseen.await.unwrap());
}

/// Hold a comment on the watched PR for an agent that has not taken it,
/// past a read of the PR merged.
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
    let pr = following.pr.clone();
    let merged = pull("merged", Some(MERGED));
    following.record_background(&pr, merged).await.unwrap();
    assert!(following.awaits().await);
}

#[tokio::test]
async fn the_background_poll_follows_a_merge_while_older_updates_wait() {
    let following = following(true, json!([])).await;
    let pr = following.pr.clone();
    hold_a_comment(&following).await;
    assert!(!following.awaits().await);

    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        following.record_background(&pr, snapshot).await.unwrap();
    }
    assert!(following.awaits().await);
}

#[tokio::test]
async fn a_merge_seen_while_its_default_branch_is_unknown_is_followed_by_the_next_read() {
    let following = following(true, json!([])).await;
    let pr = following.pr.clone();
    following
        .record_background(&pr, pull("open", None))
        .await
        .unwrap();
    // Without a remote, the checkout's branch names the default branch.
    let checkout = following.root.path().join("repo");
    git(&checkout, &["checkout", "-q", "--detach"]);
    let merged = || pull("merged", Some(MERGED));
    assert!(following.record_background(&pr, merged()).await.is_err());
    assert!(!following.awaits().await);

    git(&checkout, &["checkout", "-q", "main"]);
    following.record_background(&pr, merged()).await.unwrap();
    assert!(following.awaits().await);
}

#[tokio::test]
async fn a_run_the_background_poll_saw_finish_ends_its_wait_while_its_updates_wait() {
    let following = following(true, json!([listed_run("running")])).await;
    let pr = following.pr.clone();
    following.observe(&pr, pull("open", None)).await;
    let delivered = following.observe(&pr, pull("open", None)).await;
    assert!(delivered.is_empty(), "{delivered:?}");
    following.observe(&pr, pull("merged", Some(MERGED))).await;
    following.follow().await;
    let run = run_url(&following);
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
        let finishing = read(state, json!({}));
        following.record_background(&run, finishing).await.unwrap();
    }
    assert!(!following.awaits().await);
}

#[tokio::test]
async fn a_cursor_that_sees_the_merge_after_its_lookup_ended_starts_none() {
    let following = following(true, json!([])).await;
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    hold_a_comment(&following).await;
    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        following.record_background(&pr, snapshot).await.unwrap();
    }
    following.end_window().await;
    following.follow().await;
    assert!(!following.awaits().await);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 1);

    // The agent takes the held comment, and the cursor then sees the merge.
    let held = following.observe(&pr, pull("merged", Some(MERGED))).await;
    let deliveries = held.into_iter().map(|update| update.delivery).collect();
    let space = SpaceSelector::id(&id);
    let acknowledged =
        (following.links).acknowledge_pr_updates(&following.daemon, &space, deliveries);
    acknowledged.await.unwrap();
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
    let links = following.links.clone();
    let opened = following.store.run(move |db| {
        db.execute("DELETE FROM pr_cleanup WHERE space_id=?1", [&id])?;
        links.record(db, &id, Relation::Opened, LinkKind::Pr, &pr, Some("Fix"))
    });
    opened.await.unwrap();
    following
}

#[tokio::test]
async fn the_merge_of_a_pr_linked_as_opened_reports_nothing_of_the_pr() {
    let following = opening(true, json!([])).await;
    let (pr, id) = (following.pr.clone(), following.workspace.id.clone());
    for snapshot in [pull("open", None), pull("merged", Some(MERGED))] {
        following.record_background(&pr, snapshot).await.unwrap();
    }
    assert!(following.awaits().await);
    assert_eq!(following.events(EventKind::ItemActivity).await, 0);
    following.poll().await;
    let pending = following.links.pending_activity(&id, &[pr]).await;
    assert!(pending.unwrap().is_empty());
}

#[tokio::test]
async fn a_pushed_merge_whose_forge_cannot_be_found_ends_with_its_window() {
    let following = following(true, json!([listed_run("running")])).await;
    // The host answers nothing that says which forge it serves.
    let host = Server::start(|_| (404, Vec::new(), Vec::new()));
    let remote = format!("http://{}/team/repo.git", host.host);
    following.push(MERGED, remote).await;
    let space = LinkSpace::from(following.workspace.clone());
    assert!(following.links.follow_merge_runs(&space).await.is_err());
    assert!(following.awaits().await);

    following.end_window().await;
    following.follow().await;
    assert!(!following.awaits().await);
    assert_eq!(following.events(EventKind::NoMergeRuns).await, 1);
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
async fn a_pr_linked_as_opened_is_not_read_where_the_option_is_off() {
    let following = opening(false, json!([listed_run("running")])).await;
    following.poll().await;
    assert_eq!(following.pr_reads(), 0);
    assert!(!following.awaits().await);
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
