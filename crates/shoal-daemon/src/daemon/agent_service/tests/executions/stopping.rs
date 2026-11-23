use super::{bounded, fixture};
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
async fn stopping_allows_the_child_stop_and_completion_budgets() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    let mut running = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await?;
    // Pause only after fixture setup, which uses real subprocesses.
    tokio::time::pause();
    let request = StopRequest::manual();
    let stopping = manager.agents.stop_executions(
        &*manager,
        &workspace.id,
        StopPolicy::RequireCompleteProof,
        &request,
    );
    tokio::pin!(stopping);
    tokio::select! {
        biased;
        result = &mut stopping => bail!("stop ended before notification: {result:?}"),
        changed = running.stop.wait_for(|stop| *stop) => { changed?; }
    }

    // A connected wrapper may spend its grace stopping the child, then the
    // completion budget awaiting the daemon's survivor cleanup and inventories.
    tokio::time::advance(timing::EXECUTION_STOP_GRACE + timing::COMPLETION_ACK_TIMEOUT).await;
    tokio::select! {
        biased;
        result = &mut stopping => bail!("stop ended during valid completion work: {result:?}"),
        changed = running.stop.changed() => { changed?; }
    }
    let id = running.plan.id.clone();
    manager
        .agents
        .executions()
        .set_completing(&id, Completing::Verifying);
    manager
        .agents
        .finish_execution_after_scan(
            &*manager,
            running.plan.id,
            ExecutionKind::Command,
            Some(0),
            Some(Ok(process::Scan::default())),
        )
        .await?;
    stopping.await?;
    // The recorded step goes with the connection.
    assert_eq!(manager.agents.executions().completing(&id), None);
    Ok(())
}

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

#[tokio::test]
async fn shutdown_stops_connected_executions_and_refuses_new_ones() {
    let (_root, manager, workspace) = fixture("true").await;
    let mut running = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = running.plan.id.clone();
    let shutdown = tokio::spawn({
        let manager = manager.clone();
        async move { manager.agents.stop_for_shutdown().await }
    });
    bounded(running.stop.wait_for(|stop| *stop)).await.unwrap();
    assert!(
        manager
            .agents
            .executions()
            .stop_request(&id)
            .await
            .is_some_and(|request| request.save == Save::Everything)
    );
    manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap_err();
    manager
        .agents
        .finish_execution(&*manager, id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    bounded(shutdown).await.unwrap();
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
