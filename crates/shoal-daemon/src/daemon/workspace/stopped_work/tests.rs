use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::paths::StateFiles;
use crate::{
    config::permissions::{PermissionChange, PermissionsConfig},
    daemon::test_support::{chat, manager, repository},
    daemon::workspace::PermissionTargets,
    execution::{
        TrackedAgent,
        recovery::{LastSession, LastSessionExt},
    },
    protocol::SavedAgent,
};

/// The stopped `agents`, without waiting launches.
fn stopped_of(agents: &[SavedAgent]) -> PermissionTargets<'_> {
    PermissionTargets {
        agents,
        launches: &[],
    }
}

#[tokio::test]
async fn a_permission_change_changes_no_agent_when_one_is_refused() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "stopped".into(), None, None, None)
        .await
        .unwrap();
    let running = manager
        .create_workspace(&repo.id, "running".into(), None, None, None)
        .await
        .unwrap();
    let talk = chat(&manager, "talk", "chat", Some(&repo), None).await;
    manager.end_executions(&talk.id).await;
    // A stopped workspace agent and a stopped chat agent, and one the
    // running workspace never saved.
    let mut agents = Vec::new();
    for (space, name) in [
        (&workspace.id, "stopped"),
        (&talk.id, "talk"),
        (&running.id, "running"),
    ] {
        let execution = ExecutionId::from(format!("{name}-agent"));
        if name != "running" {
            let agent = TrackedAgent::named("claude");
            LastSession::save(&manager.paths, space, &agent, &execution).unwrap();
        }
        agents.push(SavedAgent {
            workspace: SpaceSelector::id(space),
            execution,
        });
    }
    let stopped = |manager: &crate::daemon::workspace::Manager, space| {
        LastSession::all(&manager.paths, space).unwrap()[0].narrowing
    };
    let change = PermissionChange::assigned("merge=direct").unwrap();

    // The running workspace saved no agent, so the others keep their own.
    let changed = manager.change_stopped_permissions(stopped_of(&agents), &change, None);
    assert!(changed.await.is_err());
    assert_eq!(stopped(&manager, &talk.id), PermissionsConfig::default());
    // A space's process gives only what it holds: no workspace's mode merges
    // directly here.
    let giver = Some(&running.id);
    let changed = manager.change_stopped_permissions(stopped_of(&agents[..2]), &change, giver);
    assert!(changed.await.is_err());
    assert_eq!(
        stopped(&manager, &workspace.id),
        PermissionsConfig::default()
    );
    // The user gives any, to a stopped chat's agent as to a workspace's.
    (manager.change_stopped_permissions(stopped_of(&agents[..2]), &change, None))
        .await
        .unwrap();
    assert_eq!(stopped(&manager, &workspace.id), change.set);
    assert_eq!(stopped(&manager, &talk.id), change.set);
}

#[tokio::test]
async fn only_stopped_agents_or_commands_are_unfinished_work() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for name in ["stopped", "exited"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        ids.push(workspace.id);
    }
    let (stopped, exited) = (&ids[0], &ids[1]);
    let agent = TrackedAgent::named("claude");
    let execution = ExecutionId::from("agent");
    crate::execution::recovery::save_record(
        &manager.paths,
        stopped,
        &execution,
        &agent,
        None,
        None,
    )
    .unwrap();
    LastSession::save(&manager.paths, exited, &agent, &execution).unwrap();

    let summaries = manager.saved_summaries(None).await.unwrap();
    let summarized = summaries
        .iter()
        .map(|summary| (&summary.space, summary.unfinished, summary.agents.len()));
    let (stopped_one, exited_one) = ((stopped, true, 1), (exited, false, 1));
    let mut expected = vec![stopped_one, exited_one];
    expected.sort();
    let mut summarized = summarized.collect::<Vec<_>>();
    summarized.sort();
    assert_eq!(summarized, expected);
    let saved = manager
        .saved_work(&SpaceSelector::id(stopped))
        .await
        .unwrap();
    let agents = saved
        .agents
        .iter()
        .map(|agent| (&agent.id, agent.agent.as_ref()));
    assert_eq!(
        agents.collect::<Vec<_>>(),
        [(&execution, Some(&"claude".into()))]
    );
    let saved = manager
        .saved_work(&SpaceSelector::id(exited))
        .await
        .unwrap();
    assert!(saved.agents.is_empty() && saved.sessions.len() == 1);
}

#[tokio::test]
async fn saved_work_larger_than_a_frame_is_read_whole() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "long".into(), None, None, None)
        .await
        .unwrap();
    let state = manager.paths.space_state(&workspace.id);
    std::fs::create_dir_all(&state).unwrap();
    let argv = ["sh", "-c", &"x".repeat(2 * crate::protocol::MAX_FRAME)];
    let record = serde_json::json!({ "argv": argv });
    std::fs::write(state.join("long.command.json"), record.to_string()).unwrap();

    let method = serde_json::json!({"saved": {"space": workspace.id}});
    let pages = crate::daemon::tests::serve_pages(&manager, method).await;
    assert!(pages.len() > 1, "{}", pages.len());
    let pieces = pages.iter().flat_map(|page| page.as_array().unwrap());
    let json: String = pieces.map(|piece| piece.as_str().unwrap()).collect();
    let saved: crate::client::saved::Saved = serde_json::from_str(&json).unwrap();
    assert_eq!(saved.commands[0].argv, argv);
}

/// A chat's stopped agent is its saved work, as a workspace's is, which
/// listings summarize and `shoal resume` reads.
#[tokio::test]
async fn a_chats_stopped_agent_is_its_saved_work() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let talk = chat(&manager, "talk", "chat", Some(&repo), None).await;
    let execution = ExecutionId::from("agent");
    crate::execution::recovery::save_record(
        &manager.paths,
        &talk.id,
        &execution,
        &TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();

    let saved = manager
        .saved_work(&SpaceSelector::id(&talk.id))
        .await
        .unwrap();
    assert_eq!(saved.agents.len(), 1);
    assert_eq!(saved.agents[0].id, execution);
    let summaries = manager.saved_summaries(None).await.unwrap();
    let summarized: Vec<_> = (summaries.iter())
        .map(|summary| (&summary.space, summary.unfinished))
        .collect();
    assert_eq!(summarized, [(&talk.id, true)]);
}
