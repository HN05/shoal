use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use rusqlite::params;
use serde_json::json;
use shoal_core::test_support::{git, global_config, repository};
use shoal_forge::test_support::Server;
use shoal_forge_types::{
    backends::ForgeKind,
    items::{link::Selection, reference::ItemRef},
    updates::{Update, UpdateKind},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::time::UnixTime;
use shoal_protocol::{model::Assigner, space_names::SpaceSelector, state::WorkspaceState};
use shoal_store::Store;

use crate::{
    Links,
    stand_in::{StandIn, record, service},
    wait::branch_report::BranchReport,
};

/// A Forgejo serving issue 3 of `team/repo` with as many comments by `dev`
/// as `comments` holds.
fn issue_forge(comments: Arc<AtomicUsize>) -> Server {
    Server::start(move |request| {
        let path = request.line.split_whitespace().nth(1).unwrap_or_default();
        let body = match path.split('?').next() {
            Some("/api/v1/repos/team/repo/issues/3") => {
                json!({"number": 3, "title": "Fix", "state": "open"})
            }
            Some("/api/v1/repos/team/repo/issues/3/comments") => {
                let count = comments.load(Ordering::SeqCst);
                (1..=count)
                    .map(|id| json!({"id": id, "user": {"login": "dev"}, "body": format!("{id}")}))
                    .collect()
            }
            _ => return (404, Vec::new(), Vec::new()),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

/// A workspace of a repository on `forge`, the URL of its issue 3, and the
/// links' service over a stand-in daemon with the workspace.
struct Served {
    _root: tempfile::TempDir,
    store: Store,
    links: Arc<Links>,
    daemon: StandIn,
    workspace: SpaceId,
    issue: ItemRef,
}

async fn served(forge: &Server) -> Served {
    let root = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
        .await
        .unwrap();
    let host = forge.host.clone();
    shoal_forge_types::hosts::learn(&host, ForgeKind::Forgejo);
    let config = format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"u\"\n");
    let path = repository(root.path(), "repo");
    let origin = format!("http://{host}/team/repo");
    git(&path, &["remote", "add", "origin", &origin]);
    let workspace = shoal_protocol::test_support::workspace("workspace", "repo", "fix", &path);
    record(&store, &workspace).await;
    let links = service(root.path(), store.clone(), &config);
    let daemon = StandIn {
        workspaces: vec![workspace.clone()],
        config: Some(global_config(&config)),
        ..StandIn::default()
    };
    Served {
        _root: root,
        store,
        links,
        daemon,
        workspace: workspace.id,
        issue: format!("{origin}/issues/3").parse().unwrap(),
    }
}

impl Served {
    /// Record a poll of the issue that started at `observed_at` on the
    /// workspace's cursor, returning the updates its watch reports.
    async fn poll(&self, observed_at: UnixTime) -> Vec<Update> {
        let all = Selection::default();
        let read = self
            .links
            .item_activity(vec![self.issue.clone().into()], &all);
        let read = read.await.unwrap();
        let start = self.links.poll_start_at(observed_at);
        let report = BranchReport::Conflicts;
        let recorded =
            (self.links).record_observed(&self.daemon, &self.workspace, read, &all, report, start);
        recorded.await.unwrap()
    }

    /// The workspace's activity on the issue still to deliver.
    async fn pending(&self) -> Vec<Update> {
        let urls = [self.issue.to_string()];
        self.links
            .pending_activity(&self.workspace, &urls)
            .await
            .unwrap()
    }

    /// Acknowledge `updates` as their watch's delivery does.
    async fn acknowledge(&self, updates: Vec<Update>) {
        let deliveries = updates.into_iter().map(|update| update.delivery).collect();
        let space = SpaceSelector::id(&self.workspace);
        let acknowledged = self
            .links
            .acknowledge_pr_updates(&self.daemon, &space, deliveries);
        acknowledged.await.unwrap();
    }

    /// Restart the workspace's watches as an agent's new session does,
    /// without a stop.
    async fn restart(&self) {
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let restarted = self
            .links
            .restart_watches(&self.daemon, &self.workspace, stopped);
        restarted.await.unwrap();
    }

    /// Link the issue as assigned in the store, as an earlier link did.
    async fn link_in_store(&self) {
        let (id, url) = (self.workspace.clone(), self.issue.to_string());
        let linked = self.store.run(move |db| {
            let sql = "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)";
            db.execute(sql, params![id, url])?;
            Ok(())
        });
        linked.await.unwrap();
    }

    /// Put the workspace in `state`, in the store and as the daemon sees it.
    async fn set_state(&mut self, state: WorkspaceState) {
        let id = self.workspace.clone();
        let set = self.store.run(move |db| {
            db.execute(
                "UPDATE workspaces SET state=?1 WHERE id=?2",
                params![state, id],
            )?;
            Ok(())
        });
        set.await.unwrap();
        self.daemon.workspaces[0].state = state;
    }
}

#[tokio::test]
async fn linking_an_issue_takes_its_comments_as_seen() {
    let comments = Arc::new(AtomicUsize::new(2));
    let forge = issue_forge(comments.clone());
    let mut served = served(&forge).await;
    // A workspace links its issue while it is set up.
    served.set_state(WorkspaceState::Preparing).await;
    let space = SpaceSelector::id(&served.workspace);
    let input = served.issue.to_string().parse().unwrap();
    let linked = (served.links).set_issue(&served.daemon, &space, &input, None, Assigner::Agent);
    linked.await.unwrap();
    served.set_state(WorkspaceState::Ready).await;
    assert!(served.pending().await.is_empty());
    let unchanged = served.poll(UnixTime::MAX).await;
    assert!(unchanged.is_empty(), "{unchanged:?}");

    comments.store(3, Ordering::SeqCst);
    let later = served.poll(UnixTime::MAX).await;
    assert_eq!(later.len(), 1, "{later:?}");
    assert!(later[0].message.starts_with("1 new comment from dev;"));
}

#[tokio::test]
async fn a_restart_drops_the_comments_queued_before_it() {
    let comments = Arc::new(AtomicUsize::new(2));
    let forge = issue_forge(comments.clone());
    let served = served(&forge).await;
    served.link_in_store().await;
    // A poll before the agent starts queues the issue's comments for it.
    let queued = served.poll(UnixTime::now()).await;
    assert_eq!(queued.len(), 1, "{queued:?}");

    served.restart().await;
    assert!(served.pending().await.is_empty());

    comments.store(3, Ordering::SeqCst);
    let later = served.poll(UnixTime::MAX).await;
    assert_eq!(later.len(), 1, "{later:?}");
    assert_eq!(later[0].kind, UpdateKind::Comment);
    assert!(later[0].message.starts_with("1 new comment from dev;"));
}

#[tokio::test]
async fn a_poll_that_began_before_a_restart_is_dropped_when_it_finishes() {
    let comments = Arc::new(AtomicUsize::new(1));
    let forge = issue_forge(comments.clone());
    let served = served(&forge).await;
    served.link_in_store().await;
    served.poll(UnixTime::now()).await;
    served.acknowledge(served.pending().await).await;
    // A poll reads the issue with one comment, then a restart reads it with
    // two, all within one second, before the poll records its read.
    let all = Selection::default();
    let early = served.links.poll_start();
    let stale = (served.links).item_activity(vec![served.issue.clone().into()], &all);
    let stale = stale.await.unwrap();
    comments.store(2, Ordering::SeqCst);
    served.restart().await;
    let report = BranchReport::Conflicts;
    let late = (served.links).record_observed(
        &served.daemon,
        &served.workspace,
        stale,
        &all,
        report,
        early,
    );
    assert!(late.await.unwrap().is_empty());

    comments.store(3, Ordering::SeqCst);
    let later = served.poll(UnixTime::MAX).await;
    assert_eq!(later.len(), 1, "{later:?}");
    assert!(later[0].message.starts_with("1 new comment from dev;"));
}

/// A Forgejo serving issue 3 of `team/repo` without comments, closed once
/// `closed` is set, whose next read of the issue after `hold` is set waits,
/// answering as the issue was when it arrived, until `hold` is cleared.
fn held_issue_forge(closed: Arc<AtomicBool>, hold: Arc<(Mutex<Hold>, Condvar)>) -> Server {
    Server::start(move |request| {
        let path = request.line.split_whitespace().nth(1).unwrap_or_default();
        let body = match path.split('?').next() {
            Some("/api/v1/repos/team/repo/issues/3") => {
                let state = if closed.load(Ordering::SeqCst) {
                    "closed"
                } else {
                    "open"
                };
                let body = json!({"number": 3, "title": "Fix", "state": state});
                let (held, released) = &*hold;
                let mut held = held.lock().unwrap();
                if *held == Hold::Next {
                    *held = Hold::Arrived;
                    released.notify_all();
                    let guard = std::time::Duration::from_secs(60);
                    let _ = released.wait_timeout_while(held, guard, |held| *held == Hold::Arrived);
                }
                body
            }
            Some("/api/v1/repos/team/repo/issues/3/comments") => json!([]),
            _ => return (404, Vec::new(), Vec::new()),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

/// Where the forge's held read of the issue is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    None,
    Next,
    Arrived,
}

#[tokio::test(flavor = "multi_thread")]
async fn a_poll_that_reads_after_a_restart_records_after_it() {
    let closed = Arc::new(AtomicBool::new(false));
    let hold = Arc::new((Mutex::new(Hold::None), Condvar::new()));
    let forge = held_issue_forge(closed.clone(), hold.clone());
    let served = served(&forge).await;
    served.link_in_store().await;
    assert!(served.poll(UnixTime::now()).await.is_empty());

    // The restart reads the issue open, and its read waits.
    *hold.0.lock().unwrap() = Hold::Next;
    let served = Arc::new(served);
    let restarting = {
        let served = served.clone();
        tokio::spawn(async move { served.restart().await })
    };
    let arrived = hold.clone();
    tokio::task::spawn_blocking(move || {
        let (held, released) = &*arrived;
        let guard = std::time::Duration::from_secs(60);
        let held = held.lock().unwrap();
        let held = released.wait_timeout_while(held, guard, |held| *held != Hold::Arrived);
        assert!(
            *held.unwrap().0 == Hold::Arrived,
            "the restart's read never arrived"
        );
    })
    .await
    .unwrap();
    // No poll records the issue while the restart reads it.
    assert!(
        served
            .links
            .gates
            .try_lock_prs([&served.workspace])
            .is_none()
    );
    // A poll that begins afterwards reads the issue closed.
    closed.store(true, Ordering::SeqCst);
    let polling = {
        let served = served.clone();
        tokio::spawn(async move { served.poll(UnixTime::now()).await })
    };
    *hold.0.lock().unwrap() = Hold::None;
    hold.1.notify_all();
    restarting.await.unwrap();
    let updates = polling.await.unwrap();
    let kinds: Vec<_> = updates.iter().map(|update| update.kind).collect();
    assert_eq!(kinds, [UpdateKind::Closed]);
    // Nothing reports the issue open again.
    served.acknowledge(updates).await;
    let pending = served.pending().await;
    assert!(pending.is_empty(), "{pending:?}");
    assert!(served.poll(UnixTime::now()).await.is_empty());
}
