use super::fixture;
use crate::daemon::executions::stopping::*;
use crate::daemon::executions::{Completing, ExecutionKind};
use crate::{
    process::identity::{self as process},
    protocol::timing,
};
use anyhow::Result;
use anyhow::{Context, bail};
use tokio::time::Instant;

#[tokio::test]
async fn stopping_still_bounds_a_wrapper_that_never_completes() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    // Connected receivers keep the daemon from signalling the recorded child,
    // which is this test process.
    let unstarted = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await?;
    let child = process::capture(std::process::id())?.context("test process identity")?;
    let mut running = Vec::new();
    for _ in 0..2 {
        let started = manager
            .agents
            .begin_execution(
                &*manager,
                workspace.id.as_str(),
                None,
                ExecutionKind::Command,
                None,
            )
            .await?;
        manager
            .agents
            .record_execution_child(started.plan.id.clone(), Some(child.clone()), child.pid)
            .await?;
        running.push(started);
    }
    let [still_running, verifying] = running.as_slice() else {
        bail!("expected two running executions");
    };
    manager
        .agents
        .executions()
        .set_completing(&verifying.plan.id, Completing::Verifying);
    tokio::time::pause();
    let started = Instant::now();
    let error = manager
        .agents
        .stop_executions(
            &*manager,
            &workspace.id,
            StopPolicy::RequireCompleteProof,
            &StopRequest::manual(),
        )
        .await
        .expect_err("a wrapper that never completes must time out");
    // Each execution the stop waited on is named with the step it was on.
    let message = error.to_string();
    for expected in [
        format!("execution {}: command not started", unstarted.plan.id),
        format!("execution {}: command still running", still_running.plan.id),
        format!(
            "execution {}: command exited, verifying that its processes stopped",
            verifying.plan.id
        ),
    ] {
        assert!(message.contains(&expected), "{message}");
    }
    assert!(
        (timing::WORKSPACE_STOP_TIMEOUT
            ..=timing::WORKSPACE_STOP_TIMEOUT + timing::WORKSPACE_STOP_POLL_INTERVAL)
            .contains(&started.elapsed())
    );
    assert_eq!(
        manager
            .spaces
            .workspace_executions(&workspace.id)
            .await?
            .len(),
        3
    );
    Ok(())
}

/// An agent wrapper that a disconnected execution's process started, as a
/// scoped `start --here` runs one, carries that execution's marker but is
/// another execution's: the stop leaves it running and counts it as no
/// survivor.
#[tokio::test]
async fn a_disconnected_stop_leaves_an_agent_wrapper_its_process_started() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    let starter = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await?;
    let id = starter.plan.id.clone();
    let mut command = crate::daemon::test_support::marked_child();
    command.env(crate::env::EXECUTION_ID, &id).process_group(0);
    let mut wrapper = crate::daemon::test_support::spawn_marked_child(&mut command);
    let identity = process::capture(wrapper.id().context("wrapper PID")?)?.context("wrapper")?;
    let agent = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(identity.clone()),
            ExecutionKind::Command,
            None,
        )
        .await?;
    let agent_id = agent.plan.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute(
                "UPDATE executions SET agent='claude' WHERE id=?1",
                [agent_id],
            )?;
            Ok(())
        })
        .await?;
    let execution = (manager.spaces.workspace_executions(&workspace.id).await?)
        .into_iter()
        .find(|execution| execution.id == id)
        .context("the starter's execution")?;

    let stopped =
        (manager
            .agents
            .stop_disconnected(&*manager, &execution, StopPolicy::Acknowledged))
        .await;
    let running = process::alive(&identity)?;
    wrapper.kill().await?;
    stopped?;
    assert!(running);
    let left = manager.spaces.workspace_executions(&workspace.id).await?;
    assert!(left.iter().all(|execution| execution.id != id));
    Ok(())
}
