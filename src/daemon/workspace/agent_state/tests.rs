use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
    },
    protocol::AgentMethod,
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
    let report =
        |state| manager.set_agent_state(&workspace.name, state, Some("agent".into()), None);

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
        .set_agent_state(&workspace.name, AgentState::Idle, None, None)
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
async fn workspaces_show_no_state_while_the_reporting_execution_is_unknown() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let id = workspace.id.clone();
    let set_execution = |state: ExecutionState| {
        let id = id.clone();
        manager.store.run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES ('agent',?1,?2)
                 ON CONFLICT(id) DO UPDATE SET state=excluded.state",
                params![id, state],
            )?;
            Ok(())
        })
    };
    set_execution(ExecutionState::Running).await.unwrap();
    let idle = manager
        .set_agent_state(
            &workspace.name,
            AgentState::Idle,
            Some("agent".into()),
            None,
        )
        .await
        .unwrap();

    set_execution(ExecutionState::Unknown).await.unwrap();
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().agent_state,
        None
    );

    // A wrapper that reattaches brings its state back.
    set_execution(ExecutionState::Running).await.unwrap();
    assert_eq!(
        manager.workspace(&workspace.id).await.unwrap().agent_state,
        Some(idle)
    );
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
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: own.id.clone(),
                execution: None,
            },
        )
        .await;
    let child = manager
        .split_workspace(&own.id, "child".into(), None)
        .await
        .unwrap();
    for (target, allowed) in [
        (&own.name, true),
        (&other.name, false),
        (&child.name, false),
    ] {
        let mut method = AgentMethod::SetAgentState {
            workspace: target.clone(),
            state: AgentState::Waiting,
            session: None,
            turn_end: None,
        };
        assert_eq!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
    // A failed turn is the agent's own; only an unscoped wrapper names the
    // execution it reports for.
    for (execution, allowed) in [(None, true), (Some("another".to_owned()), false)] {
        let mut method = AgentMethod::ReportTurnFailure {
            workspace: own.name.clone(),
            failure: TurnFailure::Overloaded,
            session: None,
            execution,
            turn_end: Default::default(),
        };
        assert_eq!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_ok(),
            allowed
        );
    }
    // The refused report leaves the child without an agent of its own, so
    // the coordinating agent still receives its activity.
    let delegated = manager.delegated_workspaces(&own.id).await.unwrap();
    assert_eq!(
        delegated.iter().map(|w| &w.name).collect::<Vec<_>>(),
        [&child.name]
    );
}

#[tokio::test]
async fn a_failed_turn_leaves_the_agent_idle() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    manager
        .set_agent_state(&workspace.name, AgentState::Working, None, None)
        .await
        .unwrap();
    let status = manager
        .report_turn_failure(
            &workspace.name,
            TurnFailure::Overloaded,
            None,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(status.state, AgentState::Idle);
}

#[tokio::test]
async fn a_forgotten_execution_keeps_its_turn_in_the_record_it_saved() {
    use crate::execution::recovery::{Record, record_path, save_record};
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES
                    ('stopped',?1,'unknown'),('exited',?1,'unknown')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    save_record(
        &manager.paths,
        &workspace.id,
        "stopped",
        "claude",
        None,
        None,
    )
    .unwrap();
    for execution in ["stopped", "exited"] {
        manager
            .set_agent_state(
                &workspace.name,
                AgentState::Idle,
                Some(execution.into()),
                None,
            )
            .await
            .unwrap();
        manager.forget_execution(execution).await.unwrap();
    }

    let path = record_path(&manager.paths, &workspace.id, "stopped");
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Idle));
    // An execution that saved no record stopped nothing to keep a turn for.
    assert!(
        !record_path(&manager.paths, &workspace.id, "exited").exists(),
        "a turn alone must not make a recovery record"
    );
}

#[tokio::test]
async fn an_ended_turn_starts_the_prompt_cache_lifetime_until_the_agent_works_again() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&manager, &temp).await;
    let hour = TurnEnd {
        prompt_cache_secs: Some(3600),
    };
    let ended = manager
        .end_agent_turn(&workspace.name, None, None, hour)
        .await
        .unwrap();
    assert_eq!(ended.state, AgentState::Idle);
    assert_eq!(ended.prompt_cache_expires_at, Some(ended.since + 3600));
    // Another idle report, such as an idle prompt's, keeps the expiry.
    let repeated = manager
        .set_agent_state(&workspace.name, AgentState::Idle, None, None)
        .await
        .unwrap();
    assert_eq!(repeated, ended);
    // Working drops it, and idle without an ended turn knows none.
    for state in [AgentState::Working, AgentState::Idle] {
        let status = manager
            .set_agent_state(&workspace.name, state, None, None)
            .await
            .unwrap();
        assert_eq!(status.prompt_cache_expires_at, None, "{state:?}");
    }
    // A turn whose agent knows no lifetime gets the default one. Working
    // first makes the turn's end start a new `since`.
    manager
        .set_agent_state(&workspace.name, AgentState::Working, None, None)
        .await
        .unwrap();
    let unknown = manager
        .end_agent_turn(&workspace.name, None, None, TurnEnd::default())
        .await
        .unwrap();
    assert_eq!(unknown.prompt_cache_expires_at, Some(unknown.since + 1800));
}

#[tokio::test]
async fn the_agents_ai_tool_decides_its_prompt_cache_lifetime() {
    let (temp, manager) = manager().await;
    manager.publish_config(crate::test_support::global_config(
        "[machine.ai.claude]\nprompt_cache_minutes = 5\n",
    ));
    let workspace = workspace(&manager, &temp).await;
    let started = manager
        .begin_execution(
            &workspace.id,
            None,
            super::super::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .track_agent(
            &started.plan.id,
            "claude",
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    let reported = TurnEnd {
        prompt_cache_secs: Some(3600),
    };
    let ended = manager
        .end_agent_turn(&workspace.name, Some(started.plan.id), None, reported)
        .await
        .unwrap();
    assert_eq!(ended.prompt_cache_expires_at, Some(ended.since + 300));
}
