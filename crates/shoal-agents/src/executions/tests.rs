use crate::stand_in::Fixture;
use crate::state::AgentState;
use shoal_protocol::space_names::SpaceSelector;

use super::*;

mod stopping;

/// A process waiting in a group of its own, marked as `execution`'s when
/// given. It runs this test binary, whose environment the scan can read where
/// a system binary's may be hidden.
fn background_process(execution: Option<&str>) -> tokio::process::Child {
    let mut command = shoal_core::test_support::marked_child();
    command.env_clear().env("SHOAL_TEST_PROCESS", "1");
    if let Some(execution) = execution {
        command.env(crate::env::EXECUTION_ID, execution);
    }
    shoal_core::test_support::spawn_marked_child(command.process_group(0))
}

/// The reattachment of the execution `started`, whose wrapper is this
/// process, naming that process as its child.
fn reattach(started: &StartedExecution, wrapper: &process::Identity) -> Reattach {
    Reattach {
        narrowing: Default::default(),
        mode: None,
        model: Default::default(),
        execution: started.plan.id.clone(),
        wrapper: wrapper.clone(),
        child: Some(wrapper.clone()),
        group_id: wrapper.pid,
        scope_token: started.plan.scope_token.clone(),
        agent: None,
        recover: false,
        running_ms: 0,
        awaiting_restore: None,
        between_runs: false,
        reports_status: false,
    }
}

/// A demo's wrapper between runs names its exited child, which the daemon
/// may already have forgotten when it went away before answering.
#[tokio::test]
async fn a_wrapper_between_runs_reattaches_after_its_child_was_forgotten() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("execution").await;
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = (fixture.agents)
        .begin_execution(
            daemon,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id.clone();
    let request = reattach(&started, &wrapper);
    drop(started);
    fixture.agents.detach_execution(&id).await;
    // The daemon released the exited child, then went away before answering.
    let forget = id.clone();
    (fixture.agents.store)
        .run(move |db| {
            db.execute(
                "UPDATE executions SET child=NULL,group_id=NULL WHERE id=?1",
                [forget],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (fixture.agents.reattach_execution(daemon, &request))
        .await
        .unwrap_err();
    let between_runs = Reattach {
        between_runs: true,
        ..request
    };
    let reattached = (fixture.agents.reattach_execution(daemon, &between_runs))
        .await
        .unwrap();
    assert_eq!(&reattached.space.id, &workspace.id);
}

#[tokio::test]
async fn setup_executions_cannot_reattach() {
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("execution").await;
    // Copying ignored files is a setup of its own.
    let configured = "setup.copy_ignored = true\n";
    fixture.configure_repository(workspace.repository(), configured);
    let wrapper = process::capture(std::process::id()).unwrap().unwrap();
    let started = (fixture.agents)
        .begin_execution(
            daemon,
            workspace.id.as_str(),
            Some(wrapper.clone()),
            ExecutionKind::Setup,
            None,
        )
        .await
        .unwrap();
    fixture.agents.detach_execution(&started.plan.id).await;
    let request = Reattach {
        child: None,
        ..reattach(&started, &wrapper)
    };
    (fixture.agents.reattach_execution(daemon, &request))
        .await
        .unwrap_err();
}

/// The turn the record of an idle agent keeps once its exit stops a
/// command it started, which runs `background` work of its own.
async fn turn_kept_after_exit(background: bool) -> Option<AgentState> {
    use crate::execution::recovery::{Record, RecordExt, record_path, save_record};
    let fixture = Fixture::new().await;
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("execution").await;
    let started = (fixture.agents)
        .begin_execution(
            daemon,
            workspace.id.as_str(),
            process::capture(std::process::id()).unwrap(),
            ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let id = started.plan.id;
    let selector = SpaceSelector::id(&workspace.id);
    (fixture.agents)
        .set_agent_state(daemon, &selector, AgentState::Idle, Some(id.clone()), None)
        .await
        .unwrap();
    // A signal stop saved the agent, which leaves a background command
    // it started running in a process group of its own.
    let paths = &fixture.agents.paths;
    let agent = shoal_protocol::executions::TrackedAgent::named("claude");
    save_record(paths, &workspace.id, &id, &agent, None, None).unwrap();
    let mut command = background_process(background.then_some(id.as_str()));

    // The agent's exit stops that command before the execution ends.
    fixture.agents.stop_exited_processes(daemon, &id).await;
    command.kill().await.unwrap();
    command.wait().await.unwrap();
    (fixture.agents)
        .finish_execution(daemon, id.clone(), ExecutionKind::Command, Some(0))
        .await
        .unwrap();

    let record = Record::load(&record_path(paths, &workspace.id, &id));
    record.unwrap().unwrap().turn
}

#[tokio::test]
async fn an_idle_agent_with_work_outside_its_group_keeps_a_working_turn() {
    assert_eq!(turn_kept_after_exit(true).await, Some(AgentState::Working));
}

#[tokio::test]
async fn an_idle_agent_without_work_outside_its_group_stays_idle() {
    assert_eq!(turn_kept_after_exit(false).await, Some(AgentState::Idle));
}
