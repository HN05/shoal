use std::time::Duration;

use anyhow::{Context as _, Result};

use crate::{
    client::transcripts::ReportedSession,
    daemon::{
        test_support::{manager, repository},
        workspace::{ExecutionKind, Manager},
    },
    execution::recovery::{self, LastSession},
    model::Workspace,
    state::AgentState,
};

/// An execution of agent `name` in `workspace` that reported `session`.
async fn agent(
    manager: &Manager,
    workspace: &Workspace,
    name: &str,
    session: &str,
) -> Result<String> {
    let id = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await?
        .plan
        .id;
    let agent = crate::execution::TrackedAgent::named(name);
    manager
        .track_agent(
            &id,
            &agent,
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    recovery::save_session(&manager.paths, &workspace.id, &id, session)?;
    Ok(id)
}

fn reported(agent: &str, session: &str, execution: &str, current: bool) -> ReportedSession {
    ReportedSession {
        agent: agent.into(),
        session: session.into(),
        execution: Some(execution.into()),
        current,
    }
}

#[tokio::test]
async fn a_workspace_lists_each_reported_session_once_the_running_ones_first() -> Result<()> {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "work".into(), None, None, None)
        .await?;
    assert_eq!(
        manager.space_sessions(&"work".into()).await?.sessions,
        Vec::new()
    );

    // A wrapper waiting to start or restore its agent runs no session.
    let waiting = agent(&manager, &workspace, "codex", "s2").await?;
    let running = agent(&manager, &workspace, "claude", "s1").await?;
    // The test process stands in for the agent; nothing here stops it.
    let child = crate::process::identity::capture(std::process::id())?.context("test process")?;
    manager
        .record_execution_child(running.clone(), Some(child.clone()), child.pid)
        .await?;
    // An agent that exited, and one whose session a running agent goes on.
    for (execution, session) in [("ended", "s3"), ("again", "s1")] {
        LastSession {
            mode: "worker".into(),
            model: Default::default(),
            execution: execution.into(),
            agent: "claude".into(),
            session: Some(session.into()),
            moved: false,
            copied: None,
            origin: None,
            stopped_by: None,
            stop: None,
        }
        .write(&manager.paths, &workspace.id)?;
    }

    let sessions = manager.space_sessions(&"work".into()).await?;
    assert_eq!(
        sessions.sessions,
        vec![
            reported("claude", "s1", &running, true),
            reported("codex", "s2", &waiting, false),
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
        .register_repository(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await?;
    assert_eq!(
        manager.space_sessions(&"planning".into()).await?.sessions,
        Vec::new()
    );

    manager
        .set_agent_state(
            &"planning".into(),
            AgentState::Idle,
            None,
            Some("c1".into()),
        )
        .await?;
    assert_eq!(
        manager.space_sessions(&"planning".into()).await?.sessions,
        vec![ReportedSession {
            agent: "claude".into(),
            session: "c1".into(),
            execution: None,
            current: true,
        }]
    );
    Ok(())
}
