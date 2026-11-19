use shoal_ids::execution_ids::ExecutionId;

use super::*;

/// A running project `acquirer` holds, set up for the workspace's agent
/// when `for_agent`.
fn held(acquirer: Acquirer, for_agent: bool) -> Recorded {
    Recorded {
        project: ContainerProject {
            workspace_id: SpaceId::from("w"),
            project: "shoal-w".into(),
            runtime: ContainerRuntime::Docker,
            compose: "compose.yaml".into(),
            state: ContainerState::Running,
            acquirer,
            reason: None,
            error: None,
        },
        for_agent,
    }
}

fn starter(acquirer: &Acquirer, agent_start: bool) -> Starter {
    Starter {
        acquirer: acquirer.clone(),
        for_agent: false,
        agent_start,
        reason: None,
    }
}

fn agent() -> Acquirer {
    Acquirer::Agent {
        execution_id: ExecutionId::generate(),
    }
}

#[test]
fn an_agents_start_takes_over_an_earlier_agents_project_but_not_the_users() {
    let (earlier, next) = (agent(), agent());
    let started = holder_after(&held(earlier, false), &starter(&next, true));
    assert_eq!(started, (next.clone(), false));
    let users = holder_after(&held(Acquirer::User, false), &starter(&next, true));
    assert_eq!(users, (Acquirer::User, false));
}

#[test]
fn an_agents_acquire_leaves_another_agents_project_and_the_users_takes_it() {
    let (holder, other) = (agent(), agent());
    let acquired = holder_after(&held(holder.clone(), false), &starter(&other, false));
    assert_eq!(acquired, (holder.clone(), false));
    let users = holder_after(&held(holder, false), &starter(&Acquirer::User, false));
    assert_eq!(users, (Acquirer::User, false));
}

#[test]
fn the_agent_setup_started_for_takes_the_project_over() {
    let started = agent();
    let set_up = holder_after(&held(Acquirer::User, true), &starter(&started, true));
    assert_eq!(set_up, (started, false));
}
