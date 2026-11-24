use crate::execution::recovery::LastSessionExt;
use shoal_ids::execution_ids::ExecutionId;
use std::time::Duration;

use anyhow::{Context as _, Result};

use crate::{
    daemon::{
        executions::ExecutionKind,
        manager::Manager,
        test_support::{manager, repository},
    },
    execution::recovery::{self, LastSession},
    model::Workspace,
    state::AgentState,
};
use shoal_protocol::transcripts::ReportedSession;

/// An execution of agent `name` in `workspace` that reported `session`.
async fn agent(
    manager: &Manager,
    workspace: &Workspace,
    name: &str,
    session: &str,
) -> Result<ExecutionId> {
    let id = tracked(manager, workspace, name).await?;
    recovery::save_session(&manager.paths, &workspace.id, &id, &session.into())?;
    Ok(id)
}

/// An execution of agent `name` in `workspace` that reported no session.
async fn tracked(manager: &Manager, workspace: &Workspace, name: &str) -> Result<ExecutionId> {
    let id = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await?
        .plan
        .id;
    let agent = shoal_protocol::executions::TrackedAgent::named(name);
    manager
        .agents
        .track_agent(
            manager,
            &id,
            &agent,
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    Ok(id)
}

fn reported(agent: &str, session: &str, execution: &str, current: bool) -> ReportedSession {
    ReportedSession {
        agent: agent.into(),
        session: session.into(),
        execution: Some(execution.into()),
        current,
        continued_from: None,
    }
}

#[tokio::test]
async fn a_restored_agent_lists_the_session_it_carries_until_it_reports_one() -> Result<()> {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "work".into(), None, None, None)
        .await?;
    let last: LastSession = serde_json::from_value(serde_json::json!({
        "execution": "old",
        "agent": "claude",
        "mode": "worker",
        "session": "s1",
    }))?;
    last.write(&manager.paths, &workspace.id)?;
    let restored = LastSession::path(&manager.paths, &workspace.id, &last.execution);
    let restoring = tracked(&manager, &workspace, "claude").await?;
    let from = std::slice::from_ref(&restored);
    recovery::carried::carry(&manager.paths, &workspace.id, &restoring, from)?;
    recovery::consume(&restored)?;

    let sessions = manager
        .spaces
        .space_sessions(&*manager, &manager.named("work").await)
        .await?;
    assert_eq!(
        sessions.sessions,
        vec![reported("claude", "s1", restoring.as_str(), false)]
    );
    Ok(())
}

#[tokio::test]
async fn a_workspace_lists_each_reported_session_once_the_running_ones_first() -> Result<()> {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "work".into(), None, None, None)
        .await?;
    assert_eq!(
        manager
            .spaces
            .space_sessions(&*manager, &manager.named("work").await)
            .await?
            .sessions,
        Vec::new()
    );

    // A wrapper waiting to start or restore its agent runs no session.
    let waiting = agent(&manager, &workspace, "codex", "s2").await?;
    let running = agent(&manager, &workspace, "claude", "s1").await?;
    // The test process stands in for the agent; nothing here stops it.
    let child = crate::process::identity::capture(std::process::id())?.context("test process")?;
    manager
        .agents
        .record_execution_child(running.clone(), Some(child.clone()), child.pid)
        .await?;
    // An agent that exited, and one whose session a running agent goes on.
    for (execution, session) in [("ended", "s3"), ("again", "s1")] {
        LastSession {
            narrowing: Default::default(),
            mode: "worker".into(),
            model: Default::default(),
            turn: None,
            execution: execution.into(),
            agent: "claude".into(),
            session: Some(session.into()),
            moved: false,
            copied: None,
            origin: None,
            stop: None,
            converted: false,
            handoff: None,
        }
        .write(&manager.paths, &workspace.id)?;
    }

    let sessions = manager
        .spaces
        .space_sessions(&*manager, &manager.named("work").await)
        .await?;
    assert_eq!(
        sessions.sessions,
        vec![
            reported("claude", "s1", running.as_str(), true),
            reported("codex", "s2", waiting.as_str(), false),
            reported("claude", "s3", "ended", false),
        ]
    );
    let agents: Vec<_> = sessions.integrations.keys().collect();
    assert_eq!(agents, ["claude", "codex"]);
    Ok(())
}

#[tokio::test]
async fn a_chat_lists_the_session_its_hooks_reported() -> Result<()> {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await?;
    assert_eq!(
        manager
            .spaces
            .space_sessions(&*manager, &manager.named("planning").await)
            .await?
            .sessions,
        Vec::new()
    );

    let selector = manager.named("planning").await;
    let chat = manager.spaces.chat(&selector).await?;
    let execution = manager.spaces.workspace_executions(&chat.id).await?[0]
        .id
        .clone();
    (manager.agents.set_agent_state(
        &*manager,
        &selector,
        AgentState::Idle,
        Some(execution.clone()),
        Some("c1".into()),
    ))
    .await?;
    assert_eq!(
        manager
            .spaces
            .space_sessions(&*manager, &selector)
            .await?
            .sessions,
        vec![ReportedSession {
            agent: "claude".into(),
            session: "c1".into(),
            execution: Some(execution),
            current: false,
            continued_from: None,
        }]
    );
    Ok(())
}
