use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
    },
    protocol::Method,
    test_support::{manager, repository},
};

async fn state_events(manager: &Manager) -> Vec<Option<AgentState>> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::AgentState => {
                Some(event.details.agent_state.unwrap().state)
            }
            _ => None,
        })
        .collect()
}

async fn workspace(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
                "INSERT INTO executions(id,workspace_id,state) VALUES ('agent',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let report = |state| manager.set_agent_state(&workspace.name, state, Some("agent".into()));

    let working = report(AgentState::Working).await.unwrap();
    // A repeated state keeps its start time and records no change.
    manager
        .store
        .run(|db| {
            db.execute("UPDATE workspace_agent_state SET since=since-60", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let again = report(AgentState::Working).await.unwrap();
    assert_eq!(again.since, working.since - 60);
    let waiting = report(AgentState::Waiting).await.unwrap();
    assert_eq!(waiting.state, AgentState::Waiting);
    assert!(waiting.since >= working.since);
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().agent_state,
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
        manager.workspace(&workspace.id).await.unwrap().agent_state,
        None
    );
    assert_eq!(
        state_events(&manager).await,
        [Some(AgentState::Working), Some(AgentState::Waiting), None]
    );

    // A report from outside a tracked execution stays until replaced, and
    // removal takes it without an event.
    manager
        .set_agent_state(&workspace.name, AgentState::Idle, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(state_events(&manager).await.len(), 4);
}

#[tokio::test]
async fn scoped_callers_report_only_for_their_own_workspace() {
    let (temp, manager) = manager().await;
    let own = workspace(&manager, &temp).await;
    let other = manager
        .create_workspace(&own.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: own.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&own.name, true), (&other.name, false)] {
        let mut method = Method::SetAgentState {
            workspace: target.clone(),
            state: AgentState::Waiting,
        };
        assert_eq!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
}
