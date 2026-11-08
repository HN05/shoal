use super::*;
use crate::client::space_ids::SpaceId;

#[test]
fn steps_failing_in_workspaces_are_recorded_for_each_workspace() {
    let mut failed = SweepFailures::default();
    failed.step("checkpoint expiry", Ok(()));
    failed.step("result expiry", Err(anyhow::anyhow!("disk is full")));
    for step in ["idle agents", "merged cleanup"] {
        let mut workspaces = SpaceFailures::default();
        workspaces.push(
            &SpaceId::from("id-a"),
            "a",
            &anyhow::anyhow!("{step} failed in a"),
        );
        if step == "idle agents" {
            workspaces.push(&SpaceId::from("id-b"), "b", &anyhow::anyhow!("bad config"));
        }
        failed.step(step, workspaces.into_result());
    }

    assert_eq!(
        failed.general().as_deref(),
        Some("result expiry: disk is full")
    );
    assert_eq!(
        failed.space(&SpaceId::from("id-a")).as_deref(),
        Some("idle agents: idle agents failed in a; merged cleanup: merged cleanup failed in a")
    );
    assert_eq!(
        failed.space(&SpaceId::from("id-b")).as_deref(),
        Some("idle agents: bad config")
    );
}

#[test]
fn a_pass_without_failures_succeeds() {
    let mut failed = SweepFailures::default();
    failed.step("idle cleanup", SpaceFailures::default().into_result());
    assert!(failed.into_result().is_ok());
}

#[test]
fn repeated_failures_are_logged_once_and_again_after_a_change() {
    let failing = |error: &str| {
        let mut failed = SweepFailures::default();
        failed.step("idle cleanup", Err(anyhow::anyhow!("{error}")));
        failed.into_result()
    };
    let mut logged = LoggedFailures::default();
    assert!(logged.new_failures(&failing("bad config")).is_some());
    assert!(logged.new_failures(&failing("bad config")).is_none());
    assert!(logged.new_failures(&failing("disk is full")).is_some());
    assert!(logged.new_failures(&Ok(())).is_none());
    assert!(logged.new_failures(&failing("disk is full")).is_some());
}
