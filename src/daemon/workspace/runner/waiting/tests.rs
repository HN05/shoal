use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::{
    forge::{backends::ForgeKind, test_support::Server},
    model::QueueEntry,
    test_support::{configure, git, manager, repository},
};

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
        .queue_entries()
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
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let listed = Arc::new(Mutex::new(vec![job("b", 9, 2), job("a", 4, 1)]));
    let server = forgejo(listed.clone());
    configure(
        &manager,
        &format!(
            "[machine.forges.\"{}\"]\ntype = \"forgejo\"\ntoken = \"user\"\n[runner]\nenabled = true\n",
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
                params![pull_id, json!({"url": url}).to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    // Jobs arrive in the order the forge queued them, each with its run.
    manager.refresh_waiting_jobs().await.unwrap();
    let queued = jobs(&manager).await;
    assert_eq!(handles(&queued), ["a", "b"]);
    let first = queued[0].job.as_ref().unwrap();
    assert_eq!(first.run.trigger, Trigger::Pull { number: 12 });
    assert_eq!(first.workspace_id.as_deref(), Some(pull.id.as_str()));
    assert_eq!(queued[0].name, "ci-rust-4");
    let second = queued[1].job.as_ref().unwrap();
    assert_eq!(second.workspace_id.as_deref(), Some(topic.id.as_str()));

    // Polling again queues nothing twice.
    manager.refresh_waiting_jobs().await.unwrap();
    assert_eq!(handles(&jobs(&manager).await), ["a", "b"]);

    // A passed-over job stays with the forge for other runners.
    manager.queue_remove("ci-rust-4", None).await.unwrap();
    manager.refresh_waiting_jobs().await.unwrap();
    assert_eq!(handles(&jobs(&manager).await), ["b"]);

    // Jobs another runner took leave the queue, and so does what was passed over.
    listed.lock().unwrap().clear();
    manager.refresh_waiting_jobs().await.unwrap();
    assert!(jobs(&manager).await.is_empty());
    let passed: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT count(*) FROM ci_passed_jobs", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(passed, 0);
}

#[tokio::test]
async fn a_workspaces_job_goes_ahead_of_its_agents_queued_restore() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
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
                db.execute(
                    "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,workspace_id,execution_id)
                     VALUES (?1,'restore',?2,?1,'claude','queued',0,?3,?1)",
                    params![id, repository, workspace],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let queued = |handle: &str, workspace: Option<String>| QueuedJob {
        job: serde_json::from_value(json!({
            "handle": handle, "id": 1, "run": 2, "name": handle, "labels": ["shoal"],
        }))
        .unwrap(),
        run: JobRun {
            url: String::new(),
            workflow: None,
            commit: "abc".into(),
            trigger: Trigger::Other,
            reference: None,
        },
        workspace_id: workspace,
    };
    let (unlinked, linked) = (queued("unlinked", None), queued("linked", Some(topic.id)));
    let id = repo.id.clone();
    manager
        .store
        .run(move |db| {
            queue_job(db, &id, &unlinked)?;
            queue_job(db, &id, &linked)
        })
        .await
        .unwrap();
    let order: Vec<String> = manager
        .queue_entries()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(order, ["first", "ci-linked-1", "restore", "ci-unlinked-1"]);
}
