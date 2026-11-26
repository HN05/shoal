use crate::daemon::scope::Access;
use crate::{
    daemon::{
        events::EventItem,
        manager::Manager,
        scope::{self, Caller},
        test_support::{manager, repository},
    },
    protocol::AgentMethod,
    state::{AgentState, ExecutionState, TurnFailure},
};
use rusqlite::params;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::space_names::SpaceSelector;

async fn state_events(manager: &Manager) -> Vec<Option<AgentState>> {
    manager
        .space_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) => match event.details.event {
                shoal_protocol::events::SpaceEvent::AgentState { agent_state } => {
                    Some(agent_state.state)
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

async fn workspace(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn changes_record_events_and_end_with_the_reporting_execution() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let selected = SpaceSelector::id(&workspace.id);
    let report = |state| {
        manager
            .agents
            .set_agent_state(&*manager, &selected, state, Some("agent".into()), None)
    };

    let working = report(AgentState::Working).await.unwrap();
    // A repeated state keeps its start time and records no change.
    manager
        .store
        .run(|db| {
            db.execute("UPDATE space_agent_state SET since=since-60", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let again = report(AgentState::Working).await.unwrap();
    assert_eq!(
        again.since,
        working.since - std::time::Duration::from_secs(60)
    );
    let waiting = report(AgentState::Waiting).await.unwrap();
    assert_eq!(waiting.state, AgentState::Waiting);
    assert!(waiting.since >= working.since);
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .agent_state,
        Some(waiting)
    );

    manager
        .store
        .run(|db| {
            db.execute("DELETE FROM executions WHERE id='agent'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .agent_state,
        None
    );
    assert_eq!(
        state_events(&manager).await,
        [Some(AgentState::Working), Some(AgentState::Waiting), None]
    );

    // A report from outside a tracked execution stays until replaced, and
    // removal takes it without an event.
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            None,
            None,
        )
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM spaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(state_events(&manager).await.len(), 4);
}

#[tokio::test]
async fn workspaces_show_no_state_while_the_reporting_execution_is_unknown() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let id = workspace.id.clone();
    let set_execution = |state: ExecutionState| {
        let id = id.clone();
        manager.store.run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,?2)
                 ON CONFLICT(id) DO UPDATE SET state=excluded.state",
                params![id, state],
            )?;
            Ok(())
        })
    };
    set_execution(ExecutionState::Running).await.unwrap();
    let idle = manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            AgentState::Idle,
            Some("agent".into()),
            None,
        )
        .await
        .unwrap();

    set_execution(ExecutionState::Unknown).await.unwrap();
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .agent_state,
        None
    );

    // A wrapper that reattaches brings its state back.
    set_execution(ExecutionState::Running).await.unwrap();
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .agent_state,
        Some(idle)
    );
}

#[tokio::test]
async fn scoped_callers_report_only_for_their_own_workspace() {
    let (temp, manager) = manager().await;
    let own = workspace(&manager, &temp).await;
    let other = manager
        .create_workspace(own.repository(), "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: own.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&own.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    for (target, allowed) in [(&own.id, true), (&other.id, false), (&child.id, false)] {
        let mut method = AgentMethod::SetAgentState {
            workspace: SpaceSelector::from(target),
            state: AgentState::Waiting,
            session: None,
            turn_end: None,
            made: None,
        };
        assert_eq!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
    // A failed turn is the agent's own; only an unscoped wrapper names the
    // execution it reports for.
    for (execution, allowed) in [(None, true), (Some(ExecutionId::from("another")), false)] {
        let mut method = AgentMethod::ReportTurnFailure {
            workspace: SpaceSelector::id(&own.id),
            failure: TurnFailure::Overloaded,
            resets_at: None,
            session: None,
            execution,
            turn_end: Default::default(),
            made: None,
        };
        assert_eq!(
            scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
    // The refused report leaves the child without an agent of its own, so
    // the coordinating agent still receives its activity.
    let delegated = manager
        .spaces
        .delegated_spaces(&*manager, &own.id)
        .await
        .unwrap();
    assert_eq!(
        delegated.iter().map(|w| &w.name).collect::<Vec<_>>(),
        [&child.name]
    );
}
