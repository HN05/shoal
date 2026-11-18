use super::*;
use crate::{
    cli::context::Context,
    model::Workspace,
    paths::Paths,
    sessions::{HostName, Origin},
    state::WorkspaceState,
};

fn space(name: &str) -> Space {
    Space::from(Workspace::new_record(
        "repository".into(),
        name.into(),
        name.into(),
        crate::model::Head::Branch(name.into()),
        WorkspaceState::Ready,
    ))
}

#[test]
fn every_agent_takes_a_place_among_all_the_command_selected() {
    let ctx = Context::new(Paths::for_test(std::path::Path::new("state")), true);
    let mut progress = Progress::new(&ctx, 4, HashMap::new());
    progress.skipped(Skipped {
        space: "gone".into(),
        agent: None,
        reason: "no agent runs there".into(),
    });
    progress.ended(&space("api"), Err(anyhow::anyhow!("claude disconnected")));
    let waits = RestartOutcome::Waits {
        agent: "claude".into(),
        waits: RestartWait::Turn,
    };
    progress.ended(&space("web"), Ok(waits));
    assert_eq!(progress.next(), "(4/4)");
    // A restart the daemon took for later is no failure.
    assert_eq!(progress.report.counts().1, 1);
    assert_eq!(progress.pending.len(), 1);
}

#[test]
fn a_restart_names_the_place_its_agent_moved_to() {
    let session = Origin::created(HostName::Zmx, "api".into());
    let moved = restarted("api", "(1/2)", None, Some(&session));
    assert!(moved.contains("in a detached zmx session"), "{moved}");
    // An agent in its earlier place, or outside a terminal host, is
    // reported without one.
    let kept = restarted("api", "(1/2)", Some(&session), Some(&session));
    assert_eq!(kept, "Restarted api (1/2)");
    assert_eq!(restarted("api", "(1/2)", None, None), kept);
}
