use rusqlite::params;
use serde_json::json;
use shoal_protocol::progress::Blocker;

use crate::daemon::{
    manager::Manager,
    scope::Caller,
    test_support::{chat, manager, repository, workspace_below},
};
use crate::model::{QueueRequest, Repository, Workspace};
use shoal_ids::{queue_entry_ids::QueueEntryId, space_ids::SpaceId};

fn caller(space: &SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

/// Queue the launch of an agent in `workspace` until the space `blocker`
/// is done.
async fn launch(manager: &Manager, workspace: &Workspace, blocker: &SpaceId) {
    let request = QueueRequest {
        blockers: vec![Blocker::Space(blocker.to_string())],
        ..crate::daemon::agent_queue::tests::launch_in(workspace, false)
    };
    manager.queue_add(request).await.unwrap();
}

/// Record the CI job `name` for the run of the spaces `for_spaces` as
/// starting, which the queue does not link again through its run's forge.
async fn job(manager: &Manager, repository: &Repository, name: &str, for_spaces: &[&SpaceId]) {
    let job = json!({
        "job": { "handle": name, "id": 1, "run": 2, "name": name, "labels": ["shoal"] },
        "run": {
            "url": "https://forge.example/team/repo/actions/runs/2",
            "workflow": null,
            "commit": "abc",
            "trigger": "other",
            "reference": null,
        },
        "space_ids": for_spaces,
    });
    let (repository, name) = (repository.id.clone(), name.to_owned());
    (manager.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job)
                 VALUES (?1,'job',?2,?3,'','starting',0,?4)",
                params![QueueEntryId::generate(), repository, name, job.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn reached(manager: &Manager, caller: &Caller) -> Vec<String> {
    let entries = manager.reachable_queue(Some(caller)).await.unwrap();
    let mut names: Vec<String> = entries.into_iter().map(|entry| entry.name.into()).collect();
    names.sort();
    names
}

/// A scope reaches a launch through its workspace and a CI job, which has
/// no space of its own, through its run's spaces. A master session reaches
/// every entry, those of top-level workspaces and default-branch jobs among
/// them.
#[tokio::test]
async fn a_scope_reaches_jobs_and_launches_through_their_spaces() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let worker = (manager.create_workspace(&repo.id, "worker".into(), None, None, None))
        .await
        .unwrap();
    let child = workspace_below(&manager, &repo, "child", &worker.id, None).await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let top = (manager.create_workspace(&repo.id, "top".into(), None, None, None))
        .await
        .unwrap();
    let below = workspace_below(&manager, &repo, "below", &child.id, None).await;
    launch(&manager, &top, &worker.id).await;
    launch(&manager, &below, &worker.id).await;
    job(&manager, &repo, "ci-child", &[&child.id]).await;
    job(&manager, &repo, "ci-main", &[]).await;
    assert_eq!(
        reached(&manager, &caller(&worker.id)).await,
        ["below", "ci-child"]
    );
    assert_eq!(
        reached(&manager, &caller(&master.id)).await,
        ["below", "ci-child", "ci-main", "top"]
    );
}

/// A scoped stop of every space takes the spaces its scope reaches but its
/// own.
#[tokio::test]
async fn a_scope_stops_every_space_it_reaches_but_its_own() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let worker = create("worker").await.unwrap();
    let other = create("other").await.unwrap();
    let child = workspace_below(&manager, &repo, "child", &worker.id, None).await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let mut spaces = Vec::new();
    for id in [&worker.id, &other.id, &child.id, &master.id] {
        spaces.push(manager.space_by_id(id).await.unwrap());
    }
    let stopped = async |caller: &Caller| {
        let below = manager.spaces_below(Some(caller), spaces.clone()).await;
        let mut names: Vec<String> = (below.unwrap().iter())
            .map(|space| space.name.to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(stopped(&caller(&worker.id)).await, ["child"]);
    assert_eq!(
        stopped(&caller(&master.id)).await,
        ["child", "other", "worker"]
    );
}
