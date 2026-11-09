use super::super::{
    Completing, ExecutionKind,
    tests::{bounded, fixture},
};
use super::*;
use anyhow::{Context, bail};

#[tokio::test]
async fn stopping_allows_the_child_stop_and_completion_budgets() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    let mut running = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await?;
    // Pause only after fixture setup, which uses real subprocesses.
    tokio::time::pause();
    let request = StopRequest::manual();
    let stopping =
        manager.stop_executions(&workspace.id, StopPolicy::RequireCompleteProof, &request);
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
        .executions
        .set_completing(&id, Completing::Verifying);
    manager
        .finish_execution_after_scan(
            running.plan.id,
            ExecutionKind::Command,
            Some(0),
            Some(Ok(process::Scan::default())),
        )
        .await?;
    stopping.await?;
    // The recorded step goes with the connection.
    assert_eq!(manager.executions.completing(&id), None);
    Ok(())
}

#[tokio::test]
async fn stopping_still_bounds_a_wrapper_that_never_completes() -> Result<()> {
    let (_root, manager, workspace) = fixture("true").await;
    // Connected receivers keep the daemon from signalling the recorded child,
    // which is this test process.
    let unstarted = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await?;
    let child = process::capture(std::process::id())?.context("test process identity")?;
    let mut running = Vec::new();
    for _ in 0..2 {
        let started = manager
            .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
            .await?;
        manager
            .record_execution_child(started.plan.id.clone(), Some(child.clone()), child.pid)
            .await?;
        running.push(started);
    }
    let [still_running, verifying] = running.as_slice() else {
        bail!("expected two running executions");
    };
    manager
        .executions
        .set_completing(&verifying.plan.id, Completing::Verifying);
    tokio::time::pause();
    let started = Instant::now();
    let error = manager
        .stop_executions(
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
    assert_eq!(manager.workspace_executions(&workspace.id).await?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn shutdown_stops_connected_executions_and_refuses_new_ones() {
    let (_root, manager, workspace) = fixture("true").await;
    let mut running = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = running.plan.id.clone();
    let shutdown = tokio::spawn({
        let manager = manager.clone();
        async move { manager.stop_for_shutdown().await }
    });
    bounded(running.stop.wait_for(|stop| *stop)).await.unwrap();
    assert!(
        manager
            .executions
            .stop_request(&id)
            .await
            .is_some_and(|request| request.save == Save::Everything)
    );
    manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap_err();
    manager
        .finish_execution(id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    bounded(shutdown).await.unwrap();
}
