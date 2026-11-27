use super::*;
use crate::execution::recovery::record_path;
use crate::model::Workspace;
use crate::stand_in::Fixture;
use shoal_daemon_base::events::{AgentEvent, EventKind};
use shoal_protocol::stop_causes::{EndSignal, SignalTarget};

/// Record execution `id` in `workspace`, with the recovery record its wrapper
/// saved for the stop it made unasked for `cause`, and no restore request.
async fn ended(fixture: &Fixture, workspace: &Workspace, id: &str, cause: &str) -> ExecutionId {
    let execution = fixture.execution(workspace, id, None).await;
    let path = record_path(&fixture.agents.paths, &workspace.id, &execution);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record =
        format!(r#"{{"agent":"claude","mode":"worker","stage":"stopped","cause":{cause}}}"#);
    std::fs::write(path, record).unwrap();
    execution
}

/// The cause the space's newest `agent_stopped` entry names, with its
/// execution.
async fn newest_stop(fixture: &Fixture, workspace: &Workspace) -> (ExecutionId, StopCause) {
    let id = workspace.id.clone();
    let stopped = (fixture.agents.store)
        .run(move |db| shoal_daemon_base::events::newest_of(db, &id, &[EventKind::AgentStopped]))
        .await
        .unwrap()
        .unwrap();
    let agent = stopped.event.agent().unwrap();
    (agent.execution_id.clone().unwrap(), agent.cause.unwrap())
}

/// A stop whose pause report never reached the daemon is logged as its
/// execution ends, in a done space whose agent no restore request brings
/// back.
#[tokio::test]
async fn a_signal_stop_whose_report_was_lost_is_logged_as_its_execution_ends() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("signalled").await;
    fixture.done(&workspace.id).await;
    let cause = r#"{"kind":"signal","signal":"SIGTERM","reached":"wrapper"}"#;
    let execution = ended(&fixture, &workspace, "signalled", cause).await;

    (fixture.agents)
        .forget_execution(&*fixture.daemon, &execution)
        .await
        .unwrap();

    let cause = StopCause::Signal {
        signal: EndSignal::Terminate,
        reached: SignalTarget::Wrapper,
    };
    assert_eq!(newest_stop(&fixture, &workspace).await, (execution, cause));
}

/// An OOM kill whose report was lost counts after the kill before it, though
/// the wrapper's end logged an exit without a code first.
#[tokio::test]
async fn an_oom_kill_whose_report_was_lost_counts_as_its_execution_ends() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("killed").await;
    let oom = r#"{"kind":"oom_kill"}"#;
    let first = ended(&fixture, &workspace, "first", oom).await;
    fixture.agents.record_oom_kill(&workspace.id, &first).await;
    let second = ended(&fixture, &workspace, "second", oom).await;
    let exited = AgentEvent {
        mode: None,
        name: "claude".into(),
        execution_id: Some(second.clone()),
        exit_code: None,
        oom_kills: 0,
        cause: None,
        source: None,
    };
    let kind = EventKind::AgentExited;
    shoal_daemon_base::events::record_agent(&fixture.agents.store, &workspace.id, kind, exited)
        .await;

    (fixture.agents)
        .forget_execution(&*fixture.daemon, &second)
        .await
        .unwrap();

    assert_eq!(
        newest_stop(&fixture, &workspace).await,
        (second.clone(), StopCause::OomKill)
    );
    let path = record_path(&fixture.agents.paths, &workspace.id, &second);
    let record = recovery::Record::load(&path).unwrap().unwrap();
    assert_eq!(record.oom_kills, 2);
}
