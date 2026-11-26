//! `shoal restore --discard-agent`. Each space's daemon discards its own, one
//! request per space, never waiting for an offline machine.
use anyhow::{Result, ensure};

use crate::cli::commands::resume::{
    self,
    targets::{Selected, Taking, Target},
};
use crate::cli::commands::space_selection::on_machine;
use crate::cli::{args::SpaceTargets, context::Context, ui};
use crate::client::assignments;
use crate::client::output::restore::DiscardedAgents;
use shoal_ids::execution_ids::ExecutionId;

pub(in crate::cli::commands) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        resume::ensure_queued()?;
    }
    let Some(selected) = Selected::load(ctx, targets, Taking::Discards).await? else {
        return Ok(0);
    };
    // A space `--all` selects may hold only agents their wrappers restore,
    // which a discard leaves; one named or picked has to discard something.
    let (spaces, named, skipped) = match selected {
        Selected::All => (Taking::Discards.in_reach(ctx).await?, false, Vec::new()),
        Selected::Named(spaces) => (spaces, true, Vec::new()),
        Selected::OnMachines { spaces, skipped } => (spaces, false, skipped),
    };
    ensure_one_here(&spaces, execution.as_ref())?;
    let mut report = DiscardedAgents {
        skipped,
        ..DiscardedAgents::default()
    };
    let mut names = Vec::new();
    for space in spaces {
        let name = space.shown();
        let discard = assignments::discard_stopped_agent(
            &ctx.paths,
            space.selector.clone(),
            execution.clone(),
        );
        match discard.await {
            Ok(discarded) if discarded.is_empty() && named => {
                let error =
                    anyhow::anyhow!("no stopped agent, commands or failed launch to discard");
                (report.failed).push(ui::failure(ctx, "discard", &name, &error));
            }
            Ok(discarded) if discarded.is_empty() => {}
            Ok(discarded) => {
                report.discarded.extend(discarded.executions);
                report.launches.extend(discarded.launch);
                names.push(on_machine(
                    &discarded.space,
                    space.machine_name().as_deref(),
                ));
            }
            Err(error) => (report.failed).push(ui::failure(ctx, "discard", &name, &error)),
        }
    }
    report_discarded(ctx, &report, &names)?;
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// Refuse `--execution`, which names an execution of this machine, unless
/// `spaces` is one space here.
fn ensure_one_here(spaces: &[Target], execution: Option<&ExecutionId>) -> Result<()> {
    if execution.is_none() {
        return Ok(());
    }
    ensure!(
        spaces.len() == 1,
        "--execution selects a stopped execution of one space"
    );
    if let Some(space) = spaces.iter().find(|space| space.machine.is_some()) {
        anyhow::bail!(
            "--execution names an execution of this machine, and {} runs on another machine; leave it out",
            space.shown()
        );
    }
    Ok(())
}

fn report_discarded(ctx: &Context, discarded: &DiscardedAgents, names: &[String]) -> Result<()> {
    ctx.show(discarded, |discarded| {
        resume::reports::skipped_lines(&discarded.skipped);
        match names {
            [] if discarded.failed.is_empty() && discarded.skipped.is_empty() => {
                outln!("No stopped agents, commands or failed launches")
            }
            [] => {}
            names => outln!("Discarded the agents of {}", names.join(", ")),
        }
    })
}
