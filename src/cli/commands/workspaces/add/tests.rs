use super::{AddOptions, AddPlan};
use crate::{model::Workspace, state::WorkspaceState};

fn plan(args: &[&str]) -> AddPlan {
    let workspace = Workspace::new_record(
        "repo".into(),
        "topic".into(),
        "/work/topic".into(),
        "topic".into(),
        WorkspaceState::Ready,
    );
    let agent = "claude".parse().expect("claude is a built-in agent");
    AddPlan::agent_in(
        &workspace,
        agent,
        None,
        args.iter().map(Into::into).collect(),
    )
}

#[test]
fn an_agent_waiting_for_input_skips_the_queue_unless_sent_to_the_background() {
    let options = AddOptions::default();
    assert!(options.starts_now(&plan(&[]), None));
    assert!(options.starts_now(&plan(&[]), Some(true)));
    assert!(!options.starts_now(&plan(&[]), Some(false)));
}

#[test]
fn an_agent_running_unattended_queues_unless_focused_or_now() {
    let unattended = plan(&["fix it"]);
    assert!(!AddOptions::default().starts_now(&unattended, None));
    // The focus setting cannot switch the limit off.
    assert!(!AddOptions::default().starts_now(&unattended, Some(true)));
    let focus = AddOptions {
        focus: true,
        ..Default::default()
    };
    assert!(focus.starts_now(&unattended, Some(false)));
    let now = AddOptions {
        now: true,
        ..Default::default()
    };
    assert!(now.starts_now(&unattended, None));
}

#[test]
fn an_agent_waiting_for_work_queues() {
    let options = AddOptions {
        after: vec!["other".into()],
        ..Default::default()
    };
    assert!(!options.starts_now(&plan(&[]), None));
}
