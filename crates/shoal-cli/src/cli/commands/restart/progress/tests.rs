use super::*;
use crate::{
    cli::context::Context,
    model::Workspace,
    paths::Paths,
    sessions::{HostName, Origin},
    state::WorkspaceState,
};

fn space(name: &str) -> Target {
    Target::from(crate::model::Space::from(Workspace::new_record(
        "repository".into(),
        name.into(),
        name.into(),
        crate::model::Head::Branch(name.into()),
        WorkspaceState::Ready,
    )))
}

/// The space `name` of the other machine `machine`.
fn elsewhere(name: &str, machine: &str) -> Target {
    let member = serde_json::from_value(serde_json::json!({
        "id": machine, "kind": "machine", "name": machine, "joined_at": 0,
        "connected": true, "hub": false,
    }));
    Target {
        machine: Some(member.unwrap()),
        ..space(name)
    }
}

#[test]
fn every_agent_takes_a_place_among_all_the_command_selected() {
    let ctx = Context::new(Paths::for_test(std::path::Path::new("state")), true);
    let mut progress = Progress::new(&ctx, 4, HashMap::new());
    progress.skipped(Skipped {
        space: "gone".into(),
        agent: None,
        machine: None,
        reason: "no agent runs there".into(),
    });
    progress.ended(&space("api"), Err(anyhow::anyhow!("claude disconnected")));
    let waits = RestartOutcome::Waits {
        agent: "claude".into(),
        waits: RestartWait::Turn,
    };
    progress.ended(&elsewhere("web", "desk"), Ok(waits));
    assert_eq!(progress.next(), "(4/4)");
    // A restart the daemon took for later is no failure.
    assert_eq!(progress.report.counts().1, 1);
    let [pending] = progress.pending.as_slice() else {
        panic!("expected one pending restart");
    };
    assert_eq!(pending.machine.as_deref(), Some("desk"));
}

#[test]
fn a_restart_names_the_place_its_agent_moved_to() {
    let session = Origin::created(HostName::Zmx, "api".into());
    let moved = restarted(("api", "api"), "(1/2)", None, Some(&session));
    assert!(moved.contains("in a detached zmx session"), "{moved}");
    // An agent in its earlier place, or outside a terminal host, is
    // reported without one.
    let kept = restarted(("api", "api"), "(1/2)", Some(&session), Some(&session));
    assert_eq!(kept, "Restarted api (1/2)");
    assert_eq!(restarted(("api", "api"), "(1/2)", None, None), kept);
}

#[test]
fn a_failed_restart_names_the_machine_of_its_space() {
    let ctx = Context::new(Paths::for_test(std::path::Path::new("state")), true);
    let mut progress = Progress::new(&ctx, 1, HashMap::new());
    let offline = anyhow::anyhow!("machine desk is offline");
    progress.ended(&elsewhere("api", "desk"), Err(offline));
    let mut failed = Vec::new();
    let report = progress.report.finish(&ctx, "", |restarted, failures| {
        failed = failures;
        Restarted {
            restarted,
            pending: Vec::new(),
            failed: Vec::new(),
            skipped: Vec::new(),
            interrupted: false,
        }
    });
    assert_eq!(report.unwrap(), 1);
    let workspaces: Vec<&str> = failed.iter().map(|f| f.workspace.as_str()).collect();
    assert_eq!(workspaces, ["api on desk"]);
}
