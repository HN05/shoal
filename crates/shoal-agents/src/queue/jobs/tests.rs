use std::path::Path;

use serde_json::json;
use shoal_store::test_support::{migrated, seeds};

use super::*;

/// A database with the repositories `a` and `b`.
fn database() -> Connection {
    let db = migrated().unwrap();
    for id in ["a", "b"] {
        let repository = shoal_protocol::test_support::repository(id, &Path::new("/").join(id));
        seeds::repository(&db, &repository).unwrap();
    }
    db
}

fn job(handle: &str) -> QueuedJob {
    serde_json::from_value(json!({
        "job": {"handle": handle, "id": 1, "run": 1, "name": "rust", "labels": ["shoal"]},
        "run": {"url": "https://forge.example/team/repo/actions/runs/1", "workflow": "ci.yml", "commit": "abc", "trigger": "other"},
    }))
    .unwrap()
}

fn queue_jobs(db: &Connection, repository: &str, handles: &[&str]) {
    for handle in handles {
        queue(db, &repository.into(), &job(handle)).unwrap();
    }
}

/// Admit the job entry of `handle`, as the queue does once it starts.
fn admit(db: &Connection, handle: &str) {
    db.execute(
        "UPDATE agent_queue SET state=?2 WHERE handle=?1",
        params![handle, QueueState::Starting],
    )
    .unwrap();
}

fn handles(entries: &[JobEntry]) -> Vec<&str> {
    let handles = entries.iter().map(|entry| entry.job.job.handle.as_str());
    handles.collect()
}

#[test]
fn the_jobs_in_a_state_are_read_in_queue_order_of_one_or_every_repository() {
    let db = database();
    queue_jobs(&db, "a", &["a1", "a2"]);
    queue_jobs(&db, "b", &["b1"]);
    admit(&db, "a1");

    let waiting = in_state(&db, QueueState::Queued, None).unwrap();
    assert_eq!(handles(&waiting), ["a2", "b1"]);
    let of_a = in_state(&db, QueueState::Queued, Some(&"a".into())).unwrap();
    assert_eq!(handles(&of_a), ["a2"]);
    assert_eq!(of_a[0].repository, "a");
    let admitted = in_state(&db, QueueState::Starting, None).unwrap();
    assert_eq!(handles(&admitted), ["a1"]);
}

#[test]
fn an_entry_removed_by_its_id_leaves_the_others() {
    let db = database();
    queue_jobs(&db, "a", &["a1", "a2"]);
    admit(&db, "a1");
    let admitted = in_state(&db, QueueState::Starting, None).unwrap();

    remove(&db, &admitted[0].id).unwrap();
    assert!(
        in_state(&db, QueueState::Starting, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        handles(&in_state(&db, QueueState::Queued, None).unwrap()),
        ["a2"]
    );
}

#[test]
fn forgetting_a_repositorys_jobs_keeps_those_of_others() {
    let db = database();
    queue_jobs(&db, "a", &["a1", "a2"]);
    queue_jobs(&db, "b", &["b1"]);
    admit(&db, "a1");

    forget_all(&db, &"a".into()).unwrap();
    let left = [QueueState::Queued, QueueState::Starting]
        .map(|state| handles(&in_state(&db, state, None).unwrap()).join(","));
    assert_eq!(left, ["b1", ""]);
}

#[test]
fn forgetting_the_unlisted_jobs_keeps_the_listed_and_the_admitted() {
    let db = database();
    queue_jobs(&db, "a", &["listed", "unlisted", "admitted"]);
    queue_jobs(&db, "b", &["other"]);
    admit(&db, "admitted");

    let listed = HashSet::from(["listed".to_owned()]);
    let kept = forget_unlisted(&db, &"a".into(), &listed).unwrap();
    assert_eq!(kept, HashSet::from(["listed".into(), "admitted".into()]));
    let waiting = in_state(&db, QueueState::Queued, None).unwrap();
    assert_eq!(handles(&waiting), ["listed", "other"]);
}
