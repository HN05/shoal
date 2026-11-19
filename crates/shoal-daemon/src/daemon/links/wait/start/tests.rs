use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use rusqlite::params;
use serde_json::json;

use crate::{
    daemon::{
        links::wait::branch_report::BranchReport,
        test_support::{configure, git, manager, repository},
    },
    forge::{
        items::{link::Selection, reference::ItemRef},
        updates::UpdateKind,
    },
    time::UnixTime,
};
use shoal_forge::test_support::Server;

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

/// A workspace of a repository on `forge` and the URL of its issue 3.
struct Served {
    _root: tempfile::TempDir,
    manager: Arc<crate::daemon::manager::Manager>,
    workspace: shoal_ids::space_ids::SpaceId,
    issue: ItemRef,
}

async fn served(forge: &Server) -> Served {
    let (root, manager) = manager().await;
    let host = forge.host.clone();
    crate::forge::hosts::learn(&host, crate::forge::backends::ForgeKind::Forgejo);
    let config = format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"u\"\n");
    configure(&manager, &config).await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "issue".into(), None, None, None)
        .await
        .unwrap();
    // Added once the workspace exists, which would otherwise fetch from it.
    let origin = format!("http://{host}/team/repo");
    git(&path, &["remote", "add", "origin", &origin]);
    Served {
        _root: root,
        manager,
        workspace: workspace.id,
        issue: format!("{origin}/issues/3").parse().unwrap(),
    }
}

#[tokio::test]
async fn linking_an_issue_takes_its_comments_as_seen() {
    let comments = Arc::new(AtomicUsize::new(2));
    let forge = issue_forge(comments.clone());
    // The root lives as long as the test.
    let Served {
        _root,
        manager,
        workspace,
        issue,
    } = served(&forge).await;
    // A workspace links its issue while it is set up.
    set_state(&manager, &workspace, "preparing").await;
    let assigner = crate::daemon::links::issue::Assigner::Agent;
    let space = shoal_protocol::space_names::SpaceSelector::id(&workspace);
    let input = issue.to_string().parse().unwrap();
    manager
        .set_issue(&space, &input, None, assigner)
        .await
        .unwrap();
    set_state(&manager, &workspace, "ready").await;
    let urls = [issue.to_string()];
    let pending = manager.pending_activity(&workspace, &urls).await;
    assert!(pending.unwrap().is_empty());
    let unchanged = poll(&manager, &workspace, &issue, UnixTime::MAX).await;
    assert!(unchanged.is_empty(), "{unchanged:?}");

    comments.store(3, Ordering::SeqCst);
    let later = poll(&manager, &workspace, &issue, UnixTime::MAX).await;
    assert_eq!(later.len(), 1, "{later:?}");
    assert!(later[0].message.starts_with("1 new comment from dev;"));
}

#[tokio::test]
async fn a_restart_drops_the_comments_queued_before_it() {
    let comments = Arc::new(AtomicUsize::new(2));
    let forge = issue_forge(comments.clone());
    // The root lives as long as the test.
    let Served {
        _root,
        manager,
        workspace,
        issue,
    } = served(&forge).await;
    let (id, url) = (workspace.clone(), issue.to_string());
    manager
        .store
        .run(move |db| {
            let sql = "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)";
            db.execute(sql, params![id, url])?;
            Ok(())
        })
        .await
        .unwrap();
    // A poll before the agent starts queues the issue's comments for it.
    let queued = poll(&manager, &workspace, &issue, UnixTime::now()).await;
    assert_eq!(queued.len(), 1, "{queued:?}");

    restart(&manager, &workspace).await;
    let urls = [issue.to_string()];
    let pending = manager.pending_activity(&workspace, &urls).await;
    assert!(pending.unwrap().is_empty());

    comments.store(3, Ordering::SeqCst);
    let later = poll(&manager, &workspace, &issue, UnixTime::MAX).await;
    assert_eq!(later.len(), 1, "{later:?}");
    assert_eq!(later[0].kind, UpdateKind::Comment);
    assert!(later[0].message.starts_with("1 new comment from dev;"));
}

#[tokio::test]
async fn a_poll_that_began_before_a_restart_is_dropped_when_it_finishes() {
    let comments = Arc::new(AtomicUsize::new(1));
    let forge = issue_forge(comments.clone());
    // The root lives as long as the test.
    let Served {
        _root,
        manager,
        workspace,
        issue,
    } = served(&forge).await;
    let (id, url) = (workspace.clone(), issue.to_string());
    manager
        .store
        .run(move |db| {
            let sql = "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)";
            db.execute(sql, params![id, url])?;
            Ok(())
        })
        .await
        .unwrap();
    poll(&manager, &workspace, &issue, UnixTime::now()).await;
    let urls = [issue.to_string()];
    let pending = manager.pending_activity(&workspace, &urls).await.unwrap();
    let deliveries = pending.into_iter().map(|update| update.delivery).collect();
    let space = shoal_protocol::space_names::SpaceSelector::id(&workspace);
    manager
        .acknowledge_pr_updates(&space, deliveries)
        .await
        .unwrap();
    // A poll reads the issue with one comment, then a restart reads it with
    // two, all within one second, before the poll records its read.
    let all = Selection::default();
    let early = manager.poll_start();
    let stale = manager
        .item_activity(vec![issue.clone()], &all)
        .await
        .unwrap();
    comments.store(2, Ordering::SeqCst);
    restart(&manager, &workspace).await;
    let report = BranchReport::Conflicts;
    let late = manager.record_observed(&workspace, stale, &all, report, early);
    assert!(late.await.unwrap().is_empty());

    comments.store(3, Ordering::SeqCst);
    let later = poll(&manager, &workspace, &issue, UnixTime::MAX).await;
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
    // The root lives as long as the test.
    let Served {
        _root,
        manager,
        workspace,
        issue,
    } = served(&forge).await;
    let (id, url) = (workspace.clone(), issue.to_string());
    manager
        .store
        .run(move |db| {
            let sql = "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)";
            db.execute(sql, params![id, url])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        poll(&manager, &workspace, &issue, UnixTime::now())
            .await
            .is_empty()
    );

    // The restart reads the issue open, and its read waits.
    *hold.0.lock().unwrap() = Hold::Next;
    let restarting = {
        let (manager, workspace) = (manager.clone(), workspace.clone());
        tokio::spawn(async move { restart(&manager, &workspace).await })
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
    assert!(manager.gates.try_lock_prs([&workspace]).is_none());
    // A poll that begins afterwards reads the issue closed.
    closed.store(true, Ordering::SeqCst);
    let polling = {
        let (manager, workspace, issue) = (manager.clone(), workspace.clone(), issue.clone());
        tokio::spawn(async move { poll(&manager, &workspace, &issue, UnixTime::now()).await })
    };
    *hold.0.lock().unwrap() = Hold::None;
    hold.1.notify_all();
    restarting.await.unwrap();
    let updates = polling.await.unwrap();
    let kinds: Vec<_> = updates.iter().map(|update| update.kind).collect();
    assert_eq!(kinds, [UpdateKind::Closed]);
    // Nothing reports the issue open again.
    let deliveries = updates.into_iter().map(|update| update.delivery).collect();
    let space = shoal_protocol::space_names::SpaceSelector::id(&workspace);
    manager
        .acknowledge_pr_updates(&space, deliveries)
        .await
        .unwrap();
    let urls = [issue.to_string()];
    let pending = manager.pending_activity(&workspace, &urls).await.unwrap();
    assert!(pending.is_empty(), "{pending:?}");
    assert!(
        poll(&manager, &workspace, &issue, UnixTime::now())
            .await
            .is_empty()
    );
}

/// Record a poll of `issue` that started at `observed_at` on the cursor of
/// the space `id`, returning the updates its watch reports.
async fn poll(
    manager: &crate::daemon::manager::Manager,
    id: &shoal_ids::space_ids::SpaceId,
    issue: &ItemRef,
    observed_at: UnixTime,
) -> Vec<crate::forge::updates::Update> {
    let all = Selection::default();
    let read = manager.item_activity(vec![issue.clone()], &all).await;
    let report = BranchReport::Conflicts;
    let start = manager.poll_start_at(observed_at);
    let recorded = manager.record_observed(id, read.unwrap(), &all, report, start);
    recorded.await.unwrap()
}

/// Put the workspace `id` in `state`.
async fn set_state(
    manager: &crate::daemon::manager::Manager,
    id: &shoal_ids::space_ids::SpaceId,
    state: &'static str,
) {
    let id = id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET state=?1 WHERE id=?2",
                params![state, id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Restart the watches of the space `id` as an agent's new session does,
/// without a stop.
async fn restart(manager: &crate::daemon::manager::Manager, id: &shoal_ids::space_ids::SpaceId) {
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    manager.restart_watches(id, stopped).await.unwrap();
}
