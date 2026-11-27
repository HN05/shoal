//! The forge's waiting jobs as the daemon's polls queue them.
use crate::forge::{
    ci::runner::{JobRun, Trigger},
    numbers::PullNumber,
};
use shoal_ids::space_ids::SpaceId;
use std::sync::{Arc, Mutex};

use rusqlite::params;
use serde_json::{Value, json};

use super::*;
use crate::{
    daemon::{
        runner_service::RunnerRecords,
        test_support::{configure, git, manager, repository},
    },
    forge::backends::ForgeKind,
    model::{QueueEntry, QueuedJob},
};
use shoal_forge::test_support::Server;
use shoal_runner::daemon::Records as _;
use shoal_store::test_support::seeds;

/// A Forgejo whose waiting jobs are `jobs`, each in the run its `run_id`
/// names: run 1 for PR 12, run 2 on branch `topic`.
fn forgejo(jobs: Arc<Mutex<Vec<Value>>>) -> Server {
    let server = Server::start(move |request| {
        let line = request.line.as_str();
        let body = if line == "GET /api/v1/version" {
            json!({"version": "17.0.1+gitea-1.22.0"})
        } else if line
            .starts_with("GET /api/v1/repos/team/repo/actions/runners/jobs?labels=shoal%2Cshoal-")
        {
            json!(*jobs.lock().unwrap())
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/1" {
            run(1, "pull_request", json!({"pull_request": {"number": 12}}))
        } else if line == "GET /api/v1/repos/team/repo/actions/runs/2" {
            run(2, "push", json!({"ref": "refs/heads/topic"}))
        } else {
            return (404, Vec::new(), b"{}".to_vec());
        };
        (200, Vec::new(), body.to_string().into())
    });
    crate::forge::hosts::learn(&server.host, ForgeKind::Forgejo);
    server
}

fn run(id: u64, event: &str, payload: Value) -> Value {
    json!({
        "id": id, "index_in_repo": id + 100, "event": event, "commit_sha": "abc",
        "workflow_id": "ci.yml", "event_payload": payload.to_string(),
    })
}

fn job(handle: &str, id: u64, run: u64) -> Value {
    json!({"handle": handle, "id": id, "run_id": run, "name": "rust", "runs_on": ["shoal"], "status": "waiting"})
}

async fn jobs(manager: &Manager) -> Vec<QueueEntry> {
    manager
        .agents
        .queue_entries(manager)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.kind == QueueKind::Job)
        .collect()
}

fn handles(entries: &[QueueEntry]) -> Vec<&str> {
    entries
        .iter()
        .map(|entry| entry.job.as_ref().unwrap().job.handle.as_str())
        .collect()
}

#[tokio::test]
async fn waiting_jobs_join_the_queue_linked_to_their_workspace_until_the_forge_drops_them() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let listed = Arc::new(Mutex::new(vec![job("b", 9, 2), job("a", 4, 1)]));
    let server = forgejo(listed.clone());
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n[runner]\nenabled = true\n",
            server.host
        ),
    )
    .await;
    let pull = manager
        .create_workspace(&repo.id, "pull".into(), None, None, None)
        .await
        .unwrap();
    let topic = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{}/team/repo", server.host),
        ],
    );
    let (repository_id, pull_id, url) = (
        repo.id.clone(),
        pull.id.clone(),
        format!("http://{}/team/repo/pulls/12", server.host),
    );
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO runners(repository_id,forge_repository,forge_id,uuid,token)
                 VALUES (?1,?2,5,'u','t')",
                params![repository_id, url.trim_end_matches("/pulls/12")],
            )?;
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![pull_id, json!({"urls": [url]}).to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Jobs arrive in the order the forge queued them, each with its run.
    refresh_waiting_jobs(&manager).await;
    let queued = jobs(&manager).await;
    assert_eq!(handles(&queued), ["a", "b"]);
    let first = queued[0].job.as_ref().unwrap();
    assert_eq!(
        first.run.trigger,
        Trigger::Pull {
            number: PullNumber::new(12)
        }
    );
    assert_eq!(first.space_ids, std::slice::from_ref(&pull.id));
    assert_eq!(queued[0].name, "ci-rust-4");
    let second = queued[1].job.as_ref().unwrap();
    assert_eq!(second.space_ids, std::slice::from_ref(&topic.id));

    // Polling again queues nothing twice.
    refresh_waiting_jobs(&manager).await;
    assert_eq!(handles(&jobs(&manager).await), ["a", "b"]);

    // A passed-over job stays with the forge for other runners.
    manager
        .agents
        .queue_remove(&*manager, &manager.queued("ci-rust-4").await, None)
        .await
        .unwrap();
    refresh_waiting_jobs(&manager).await;
    assert_eq!(handles(&jobs(&manager).await), ["b"]);

    // Jobs another runner took leave the queue, and so does what was passed over.
    listed.lock().unwrap().clear();
    refresh_waiting_jobs(&manager).await;
    assert!(jobs(&manager).await.is_empty());
    let passed: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_passed_jobs", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(passed, 0);
}

#[tokio::test]
async fn a_job_ranks_with_its_workspace_ahead_of_that_workspaces_restore() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let topic = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let ids = (repo.id.clone(), topic.id.clone(), other.id.clone());
    manager
        .store
        .run(move |db| {
            let (repository, topic, other) = &ids;
            for (id, workspace) in [("first", other), ("restore", topic)] {
                let entry = seeds::QueueEntry {
                    repository: Some(repository.clone()),
                    space: Some(workspace.clone()),
                    execution: Some(id.into()),
                    ..seeds::QueueEntry::new(id, QueueKind::Restore, QueueState::Queued)
                };
                seeds::queue_entry(db, &entry)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    // A settled forge relinks each job without asking the host.
    configure(
        &manager,
        "[machine.forges.\"forge.example\"]\ntype = \"forgejo\"\n",
    )
    .await;
    let queued = |handle: &str, trigger, space_ids: Vec<SpaceId>| QueuedJob {
        job: serde_json::from_value(json!({
            "handle": handle, "id": 1, "run": 2, "name": handle, "labels": ["shoal"],
        }))
        .unwrap(),
        run: JobRun {
            url: "https://forge.example/team/repo/actions/runs/2"
                .parse()
                .unwrap(),
            workflow: None,
            commit: "abc".into(),
            trigger,
            reference: None,
        },
        space_ids,
        default_branch: false,
    };
    let (unlinked, linked) = (
        queued("unlinked", Trigger::Other, Vec::new()),
        queued("linked", Trigger::Branch("topic".into()), vec![topic.id]),
    );
    let id = repo.id.clone();
    manager
        .store
        .write(move |tx| {
            RunnerRecords.queue_job(tx, &id, &unlinked)?;
            RunnerRecords.queue_job(tx, &id, &linked)
        })
        .await
        .unwrap();
    let order: Vec<String> = manager
        .agents
        .queue_entries(&*manager)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name.into())
        .collect();
    // The job ranks with the started workspace it serves, ahead of that
    // workspace's agent; the job serving none is new work.
    let place = |name: &str| order.iter().position(|entry| entry == name).unwrap();
    assert!(place("ci-linked-1") < place("restore"));
    assert_eq!(order.last().map(String::as_str), Some("ci-unlinked-1"));
}

#[tokio::test]
async fn a_waiting_job_serves_the_workspace_that_links_its_pr_after_it_queued() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let server = forgejo(Arc::new(Mutex::new(vec![job("a", 4, 1)])));
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n[runner]\nenabled = true\n",
            server.host
        ),
    )
    .await;
    // Created before the remote, so it starts from the local default branch.
    let pull = manager
        .create_workspace(&repo.id, "pull".into(), None, None, None)
        .await
        .unwrap();
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            &format!("http://{}/team/repo", server.host),
        ],
    );
    let (repository_id, forge_repository) =
        (repo.id.clone(), format!("http://{}/team/repo", server.host));
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO runners(repository_id,forge_repository,forge_id,uuid,token)
                 VALUES (?1,?2,5,'u','t')",
                params![repository_id, forge_repository],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    refresh_waiting_jobs(&manager).await;
    let entry = jobs(&manager).await.remove(0);
    assert!(entry.job.as_ref().unwrap().space_ids.is_empty());

    let (pull_id, url) = (
        pull.id.clone(),
        format!("http://{}/team/repo/pulls/12", server.host),
    );
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                params![pull_id, json!({"urls": [url]}).to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let linked = manager.runner.waiting_job_spaces(&*manager).await.unwrap();
    assert_eq!(linked[&entry.id], [pull.id]);
}
