use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::{global_config, manager},
        workspace::{
            Manager,
            agent_queue::tests::{connected_agent, create, register, request},
        },
    },
    model::{QueueRequest, Repository, Workspace},
};

/// Queue a launch of the agent in `workspace`, as a worker start does.
async fn launch_in(
    manager: &Manager,
    repository: &Repository,
    workspace: &Workspace,
) -> anyhow::Result<()> {
    manager
        .queue_add(QueueRequest {
            workspace: Some(SpaceSelector::id(&workspace.id)),
            ..request(repository, workspace.name.as_str(), false)
        })
        .await
        .map(drop)
}

/// The refusal of a second worker, which names the command that frees the
/// workspace.
fn refusal(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.expect_err("a second worker was queued"))
}

#[tokio::test]
async fn a_running_agent_refuses_another_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "busy").await;
    let _agent = connected_agent(&manager, &workspace).await;
    let refused = refusal(launch_in(&manager, &repo, &workspace).await);
    assert!(refused.contains("shoal attach"), "{refused}");
    assert!(manager.queue_entries().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_queued_agent_refuses_another_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let workspace = create(&manager, &repo, "waiting").await;
    launch_in(&manager, &repo, &workspace).await.unwrap();
    let refused = refusal(launch_in(&manager, &repo, &workspace).await);
    assert!(refused.contains("shoal queue"), "{refused}");
    assert_eq!(manager.queue_entries().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_stopped_agent_refuses_another_worker_until_it_is_resumed_or_discarded() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "stopped").await;
    let execution = ExecutionId::from("stopped-agent".to_owned());
    let record = crate::execution::recovery::save_record(
        &manager.paths,
        &workspace.id,
        &execution,
        &crate::execution::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let refused = refusal(launch_in(&manager, &repo, &workspace).await);
    assert!(refused.contains("shoal resume --discard"), "{refused}");
    std::fs::remove_file(record).unwrap();
    launch_in(&manager, &repo, &workspace).await.unwrap();
}

#[tokio::test]
async fn a_workspace_without_an_agent_takes_a_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "idle").await;
    launch_in(&manager, &repo, &workspace).await.unwrap();
}

#[tokio::test]
async fn a_launch_takes_a_workspace_it_did_not_name_only_while_it_has_no_agent() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // Started now, as a terminal's start is, without naming a workspace.
    let admission = manager
        .queue_add(request(&repo, "topic", true))
        .await
        .unwrap();
    let entry = admission.entry.unwrap().id;
    // The branch gained a workspace and an agent meanwhile.
    let busy = create(&manager, &repo, "busy").await;
    let _agent = connected_agent(&manager, &busy).await;
    let refused = manager
        .queue_attach(&entry, &SpaceSelector::id(&busy.id))
        .await;
    assert!(refused.is_err());
    let free = create(&manager, &repo, "free").await;
    manager
        .queue_attach(&entry, &SpaceSelector::id(&free.id))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_start_reopening_a_workspace_that_gained_an_agent_is_refused() {
    use crate::daemon::operations::starts::{Opening, Preparing};
    use crate::daemon::workspace::agent_queue::HasAgent;
    use crate::protocol::operations::WorkspaceStart;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // Admitted on a branch that had no workspace yet.
    let admission = manager
        .queue_add(request(&repo, "busy", true))
        .await
        .unwrap();
    let entry = admission.entry.unwrap().id;
    // The branch gained a workspace and an agent meanwhile.
    let busy = create(&manager, &repo, "busy").await;
    let _agent = connected_agent(&manager, &busy).await;
    let opening = Opening {
        start: WorkspaceStart::Branch(busy.branch_or_name().to_owned()),
        base: None,
        new: Default::default(),
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: Some(entry),
        env: None,
    };
    let refused = manager
        .start_workspace((&repo.id).into(), opening, preparing, None)
        .await
        .expect_err("a second worker started");
    assert!(refused.is::<HasAgent>(), "{refused:#}");
    let entries = manager.queue_entries().await.unwrap();
    assert!(
        entries.iter().all(|entry| entry.workspace_id.is_none()),
        "{entries:?}"
    );
}

#[tokio::test]
async fn overlapping_unlimited_starts_on_an_existing_branch_take_one_worker() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[machine]\nmax_agents = 'none'"));
    let on_topic = || QueueRequest {
        plan: r#"{"creation":{"existing":"topic"}}"#.into(),
        ..request(&repo, "topic", true)
    };
    // No limit applies, yet the start that may reopen the branch's
    // workspace keeps an entry for its one agent.
    let first = manager.queue_add(on_topic()).await.unwrap();
    assert!(first.start);
    assert!(first.entry.is_some());
    let second = manager.queue_add(on_topic()).await;
    assert!(second.is_err(), "{second:?}");
}

#[tokio::test]
async fn an_agent_without_a_launch_waits_out_one_queued_in_its_workspace() {
    use crate::daemon::workspace::agent_queue::HasAgent;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "worked").await;
    let blocker = create(&manager, &repo, "blocker").await;
    let selector = SpaceSelector::id(&workspace.id);
    // Nothing waits there, so a restored agent may register.
    drop(manager.reserve_worker(&selector).await.unwrap());
    // A launch waits there for another space's work.
    let admission = manager
        .queue_add(QueueRequest {
            workspace: Some(selector.clone()),
            blockers: vec![crate::model::Blocker::Space(blocker.id.to_string())],
            ..request(&repo, "worked", false)
        })
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    let refused = manager.reserve_worker(&selector).await.map(drop);
    assert!(
        refused.as_ref().is_err_and(|error| error.is::<HasAgent>()),
        "{refused:?}"
    );
    // Once it leaves the queue, the restore may go ahead.
    manager.queue_remove(entry.id.as_str(), None).await.unwrap();
    drop(manager.reserve_worker(&selector).await.unwrap());
}
