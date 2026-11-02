use super::*;

#[test]
fn steps_failing_in_workspaces_are_recorded_for_each_workspace() {
    let mut failed = SweepFailures::default();
    failed.step("checkpoint expiry", Ok(()));
    failed.step("result expiry", Err(anyhow::anyhow!("disk is full")));
    for step in ["idle agents", "merged cleanup"] {
        let mut workspaces = SpaceFailures::default();
        workspaces.push("id-a", "a", &anyhow::anyhow!("{step} failed in a"));
        if step == "idle agents" {
            workspaces.push("id-b", "b", &anyhow::anyhow!("bad config"));
        }
        failed.step(step, workspaces.into_result());
    }

    assert_eq!(
        failed.general().as_deref(),
        Some("result expiry: disk is full")
    );
    assert_eq!(
        failed.space("id-a").as_deref(),
        Some("idle agents: idle agents failed in a; merged cleanup: merged cleanup failed in a")
    );
    assert_eq!(
        failed.space("id-b").as_deref(),
        Some("idle agents: bad config")
    );
    assert_eq!(
        failed.to_string(),
        "result expiry: disk is full; \
         a: idle agents: idle agents failed in a; merged cleanup: merged cleanup failed in a; \
         b: idle agents: bad config"
    );
}

#[test]
fn a_pass_without_failures_succeeds() {
    let mut failed = SweepFailures::default();
    failed.step("idle cleanup", SpaceFailures::default().into_result());
    assert!(failed.into_result().is_ok());
}
