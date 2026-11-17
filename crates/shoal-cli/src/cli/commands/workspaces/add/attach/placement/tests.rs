use super::starts_now;
use crate::cli::commands::workspaces::AddOptions;
use shoal_protocol::launches::Launch;

/// A launch whose agent is told `task`, or waits for the user without one.
fn told(task: &[&str]) -> Launch {
    Launch {
        task: task.iter().map(|&arg| arg.into()).collect(),
        ..Launch::default()
    }
}

#[test]
fn an_agent_waiting_for_input_skips_the_queue_unless_sent_to_the_background() {
    let options = AddOptions::default();
    assert!(starts_now(&told(&[]), None, &options, true));
    assert!(starts_now(&told(&[]), Some(true), &options, true));
    assert!(!starts_now(&told(&[]), Some(false), &options, true));
}

#[test]
fn an_agent_started_without_a_terminal_queues() {
    assert!(!starts_now(&told(&[]), None, &AddOptions::default(), false));
}

#[test]
fn an_agent_running_unattended_queues_unless_focused() {
    let unattended = told(&["fix it"]);
    assert!(!starts_now(&unattended, None, &AddOptions::default(), true));
    // The focus setting cannot switch the limit off.
    assert!(!starts_now(
        &unattended,
        Some(true),
        &AddOptions::default(),
        true
    ));
    let focus = AddOptions {
        focus: true,
        ..Default::default()
    };
    assert!(starts_now(&unattended, Some(false), &focus, true));
}

#[test]
fn an_agent_waiting_for_work_queues() {
    let blocked = Launch {
        blockers: vec!["other".into()],
        ..told(&[])
    };
    assert!(!starts_now(&blocked, None, &AddOptions::default(), true));
}
