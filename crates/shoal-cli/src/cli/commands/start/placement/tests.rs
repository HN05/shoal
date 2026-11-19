use super::{starts_now, waits};
use crate::cli::commands::workspaces::AddOptions;
use shoal_protocol::launches::Launch;

/// A launch whose agent is told `task`, or waits for the user without one.
fn told(task: &[&str]) -> Launch {
    Launch {
        task: task.iter().map(|&arg| arg.into()).collect(),
        ..Launch::default()
    }
}

/// Whether the agent of `launch` skips the queue, started from a call
/// `on_terminal`, on no issue a workspace links.
fn now(launch: &Launch, setting: Option<bool>, options: &AddOptions, on_terminal: bool) -> bool {
    starts_now(
        launch,
        setting,
        options,
        on_terminal && waits(launch, false),
    )
}

#[test]
fn an_agent_waiting_for_input_skips_the_queue_unless_sent_to_the_background() {
    let options = AddOptions::default();
    assert!(now(&told(&[]), None, &options, true));
    assert!(now(&told(&[]), Some(true), &options, true));
    assert!(!now(&told(&[]), Some(false), &options, true));
}

#[test]
fn an_agent_started_without_a_terminal_queues() {
    assert!(!now(&told(&[]), None, &AddOptions::default(), false));
}

#[test]
fn an_agent_running_unattended_queues_unless_focused() {
    let unattended = told(&["fix it"]);
    assert!(!now(&unattended, None, &AddOptions::default(), true));
    // The focus setting cannot switch the limit off.
    assert!(!now(&unattended, Some(true), &AddOptions::default(), true));
    let focus = AddOptions {
        focus: true,
        ..Default::default()
    };
    assert!(now(&unattended, Some(false), &focus, true));
}

#[test]
fn an_agent_waiting_for_work_queues() {
    let blocked = Launch {
        blockers: vec!["other".into()],
        ..told(&[])
    };
    assert!(!now(&blocked, None, &AddOptions::default(), true));
}

#[test]
fn a_workspaces_linked_issue_prompts_its_worker_as_a_named_one_does() {
    assert!(waits(&told(&[]), false));
    assert!(!waits(&told(&[]), true));
}
