use super::{AddOptions, AddPlan};
use crate::{model::Workspace, protocol::operations::QueuedLaunch, state::WorkspaceState};

fn plan(args: &[&str]) -> AddPlan {
    let workspace = Workspace::new_record(
        "repo".into(),
        "topic".into(),
        "/work/topic".into(),
        Some("topic".into()),
        WorkspaceState::Ready,
    );
    let launch = QueuedLaunch {
        agent: "claude".into(),
        args: args.iter().map(|&arg| arg.into()).collect(),
        ..Default::default()
    };
    AddPlan::agent_in(&workspace, launch).expect("claude is a built-in agent")
}

#[test]
fn an_agent_waiting_for_input_skips_the_queue_unless_sent_to_the_background() {
    let options = AddOptions::default();
    assert!(options.starts_now(&plan(&[]), None, true));
    assert!(options.starts_now(&plan(&[]), Some(true), true));
    assert!(!options.starts_now(&plan(&[]), Some(false), true));
}

#[test]
fn an_agent_added_without_a_terminal_queues() {
    assert!(!AddOptions::default().starts_now(&plan(&[]), None, false));
}

#[test]
fn an_agent_running_unattended_queues_unless_focused_or_now() {
    let unattended = plan(&["fix it"]);
    assert!(!AddOptions::default().starts_now(&unattended, None, true));
    // The focus setting cannot switch the limit off.
    assert!(!AddOptions::default().starts_now(&unattended, Some(true), true));
    let focus = AddOptions {
        focus: true,
        ..Default::default()
    };
    assert!(focus.starts_now(&unattended, Some(false), true));
    let now = AddOptions {
        now: true,
        ..Default::default()
    };
    assert!(now.starts_now(&unattended, None, true));
}

#[test]
fn an_agent_waiting_for_work_queues() {
    let options = AddOptions {
        after: vec!["other".into()],
        ..Default::default()
    };
    assert!(!options.starts_now(&plan(&[]), None, true));
}

#[test]
fn a_finished_dependency_s_branch_is_the_base_of_new_branches_but_not_of_copies() {
    let mut new = super::Creation::default();
    new.build_on(Some("dependency".into()));
    assert_eq!(new.base.as_deref(), Some("dependency"));
    let mut copy = super::Creation {
        copy: Some(super::CopyCreation {
            source: crate::protocol::CopySource {
                workspace: "topic".into(),
                checkpoint: None,
            },
            name: "topic-copy".into(),
        }),
        ..Default::default()
    };
    copy.build_on(Some("dependency".into()));
    assert_eq!(copy.base, None);
}
