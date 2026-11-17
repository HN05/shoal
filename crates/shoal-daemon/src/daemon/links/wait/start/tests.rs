use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
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
    manager: Arc<crate::daemon::workspace::Manager>,
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

    manager.restart_watches(&workspace).await.unwrap();
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
    manager.restart_watches(&workspace).await.unwrap();
    let report = BranchReport::Conflicts;
    let late = manager.record_observed(&workspace, stale, &all, report, early);
    assert!(late.await.unwrap().is_empty());

    comments.store(3, Ordering::SeqCst);
    let later = poll(&manager, &workspace, &issue, UnixTime::MAX).await;
    assert_eq!(later.len(), 1, "{later:?}");
    assert!(later[0].message.starts_with("1 new comment from dev;"));
}

/// Record a poll of `issue` that started at `observed_at` on the cursor of
/// the space `id`, returning the updates its watch reports.
async fn poll(
    manager: &crate::daemon::workspace::Manager,
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
    manager: &crate::daemon::workspace::Manager,
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
