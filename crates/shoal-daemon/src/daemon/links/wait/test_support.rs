//! A forge that serves nothing, the items the tests link on it, and the
//! activity the tests record on them as a background poll would.
use crate::forge::updates::SnapshotState;
use rusqlite::params;
use serde_json::json;
use shoal_forge::updates::Snapshot;
use shoal_ids::space_ids::SpaceId;
use std::sync::LazyLock;

use crate::daemon::{links::pr::RegistrationKind, manager::Manager};

/// A forge that serves nothing, which every item the tests link belongs
/// to, so a lookup of one fails at once.
static FORGE: LazyLock<shoal_forge::test_support::Server> = LazyLock::new(|| {
    let server = shoal_forge::test_support::Server::start(|_| (404, Vec::new(), Vec::new()));
    crate::forge::hosts::learn(&server.host, crate::forge::backends::ForgeKind::Forgejo);
    server
});

/// The URL of the item at `path` in the repository on [`FORGE`].
pub(crate) fn item_url(path: &str) -> String {
    format!("http://{}/team/repo/{path}", FORGE.host)
}

/// An open item whose one comment reads `message`.
pub(crate) fn snapshot(message: &str) -> Snapshot {
    let record = json!({"comments": {"one": {"fingerprint": message}}, "checks": {},
        "conflict": null, "errors": {}});
    Snapshot::stored(Some(SnapshotState::Open), &record.to_string()).unwrap()
}

/// Have the space `id` watch the item at `url`.
pub(crate) async fn register_watch(manager: &Manager, id: &SpaceId, url: &str) {
    let id = id.clone();
    let registration = crate::daemon::links::pr::Registration {
        kind: RegistrationKind::Watch {
            urls: vec![url.parse().unwrap()],
            merged_head: None,
        },
        error: None,
    };
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT OR REPLACE INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![id, serde_json::to_string(&registration)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Have the space `id` watch a PR on which a comment then arrives, as a
/// background poll finds it.
pub async fn comment_on_watched_pr(manager: &Manager, id: &SpaceId) {
    let pr = item_url("pulls/1");
    register_watch(manager, id, &pr).await;
    poll_watched(manager, id, &pr, snapshot("Please rebase")).await;
}

/// Have the space `id` watch a PR that background polls first find clean,
/// then conflicting with a new base, twice, as a forge confirms a conflict.
pub async fn conflict_on_watched_pr(manager: &Manager, id: &SpaceId) {
    let pr = item_url("pulls/1");
    register_watch(manager, id, &pr).await;
    for (base, conflict) in [('b', false), ('c', true), ('c', true)] {
        poll_watched(manager, id, &pr, pr_at(base, conflict)).await;
    }
}

/// An open PR at the base commit spelled by `base`, conflicting with it or
/// not.
fn pr_at(base: char, conflict: bool) -> Snapshot {
    let (head, base) = ("a".repeat(40), base.to_string().repeat(40));
    serde_json::from_value(json!({
        "comments": {}, "checks": {}, "state": "open", "errors": {},
        "head": head, "base": base, "conflict": conflict,
        "conflict_at": {"head": head, "base": base},
    }))
    .unwrap()
}

/// Record `snapshot` of the PR at `url` as a background poll of the space
/// `id` observes it.
async fn poll_watched(manager: &Manager, id: &SpaceId, url: &str, snapshot: Snapshot) {
    let observed = vec![(url.to_owned(), snapshot)];
    let start = manager.links.poll_start();
    (manager.links)
        .record_background_activity(manager, id, observed.clone())
        .await
        .unwrap();
    (manager.links)
        .queue_background_updates(manager, id, observed, start)
        .await
        .unwrap();
}
