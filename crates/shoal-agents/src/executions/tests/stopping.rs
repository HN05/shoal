use anyhow::{Result, bail};

use super::*;
use crate::executions::stopping::{Save, StopPolicy, StopRequest};
use crate::protocol::timing;

/// A hang guard, not a deadline: the awaited step has already begun.
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("an execution step hung")
}

#[tokio::test]
async fn stopping_allows_the_child_stop_and_completion_budgets() -> Result<()> {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("execution").await;
    let mut running = (fixture.agents)
        .begin_execution(
            daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
        .await?;
    // Pause only after fixture setup, which uses real subprocesses.
    tokio::time::pause();
    let request = StopRequest::manual();
    let stopping = fixture.agents.stop_executions(
        daemon,
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
    let executions = fixture.agents.executions();
    executions.set_completing(&id, Completing::Verifying);
    (fixture.agents)
        .finish_execution_after_scan(
            daemon,
            running.plan.id,
            ExecutionKind::Command,
            Some(0),
            Some(Ok(process::Scan::default())),
        )
        .await?;
    stopping.await?;
    // The recorded step goes with the connection.
    assert_eq!(executions.completing(&id), None);
    Ok(())
}

#[tokio::test]
async fn shutdown_stops_connected_executions_and_refuses_new_ones() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("execution").await;
    let begin = || {
        (fixture.agents).begin_execution(
            daemon,
            workspace.id.as_str(),
            None,
            ExecutionKind::Command,
            None,
        )
    };
    let mut running = begin().await.unwrap();
    let id = running.plan.id.clone();
    let shutdown = tokio::spawn({
        let agents = fixture.agents.clone();
        async move { agents.stop_for_shutdown().await }
    });
    bounded(running.stop.wait_for(|stop| *stop)).await.unwrap();
    let request = fixture.agents.executions().stop_request(&id).await;
    assert!(request.is_some_and(|request| request.save == Save::Everything));
    begin().await.unwrap_err();
    (fixture.agents)
        .finish_execution(daemon, id, ExecutionKind::Command, Some(0))
        .await
        .unwrap();
    bounded(shutdown).await.unwrap();
}
