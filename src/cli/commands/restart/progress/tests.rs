use super::*;
use crate::{cli::context::Context, model::Workspace, paths::Paths, state::WorkspaceState};

fn restart(name: &str) -> Restart {
    let workspace = Workspace::new_record(
        "repository".into(),
        name.into(),
        name.into(),
        Some(name.into()),
        WorkspaceState::Ready,
    );
    Restart {
        agent: RunningAgent {
            execution: format!("{name}-execution"),
            workspace_id: workspace.id.clone(),
            agent: Some("claude".into()),
            restorable: true,
            state: None,
            interrupted: false,
            origin: None,
            restarted_by: Vec::new(),
            restart: None,
            stopped: None,
            restarted_from: Vec::new(),
        },
        workspace,
        joins: None,
    }
}

#[test]
fn every_agent_takes_a_place_among_all_the_run_selected() {
    let ctx = Context::new(Paths::for_test(std::path::Path::new("state")), true);
    let api = restart("api");
    let mut progress = Progress::new(std::slice::from_ref(&api), 2);
    progress.skip(
        &ctx,
        Skipped {
            workspace: "gone".into(),
            agent: None,
            reason: "no agent runs there".into(),
        },
    );
    progress.ended(&ctx, &api, Err(anyhow::anyhow!("claude disconnected")));
    assert_eq!(progress.next(), "(3/3)");
}

#[test]
fn an_interruption_names_the_restarts_that_go_on() {
    assert!(!interruption(&[], None).contains("shoal restart"));
    let pending = ["api".into(), "web".into()];
    let plain = interruption(&pending, None);
    assert!(plain.contains("shoal restart api web "), "{plain}");
    // The command keeps to the agents the run selected.
    let selected = interruption(&pending, Some("codex"));
    assert!(
        selected.contains("shoal restart api web --agent codex "),
        "{selected}"
    );
}
