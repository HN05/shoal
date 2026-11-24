//! `shoal restore --discard-agent`: discarding a space's stopped agent, last
//! sessions, stopped commands and failed launch instead of bringing them
//! back, which leaves its files, branch and worktree as they are. Each
//! space's daemon discards its own, one request per space.
use anyhow::{Result, ensure};

use crate::cli::commands::resume::{self, targets};
use crate::cli::{args::SpaceTargets, context::Context, ui};
use crate::client::assignments;
use crate::client::output::restore::DiscardedAgents;
use shoal_ids::execution_ids::ExecutionId;

/// Discard the agents of the spaces `targets` selects as `shoal resume`
/// selects them, every space's with one for `--all`, only the `execution`
/// one when given.
pub(in crate::cli::commands) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    execution: Option<ExecutionId>,
) -> Result<i32> {
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        resume::ensure_queued()?;
    }
    let Some(selected) = targets::Selected::load(ctx, targets).await? else {
        return Ok(0);
    };
    // A space `--all` selects may hold only agents their wrappers restore,
    // which a discard leaves; one named or picked has to discard something.
    let named = selected.spaces.is_some();
    let spaces = match selected.spaces {
        Some(spaces) => spaces,
        None => targets::discardable_in_reach(ctx).await?,
    };
    ensure!(
        execution.is_none() || spaces.len() == 1,
        "--execution selects a stopped execution of one space"
    );
    let listed = crate::client::spaces::list(&ctx.paths).await?;
    let mut report = DiscardedAgents::default();
    let mut names = Vec::new();
    for selector in spaces {
        let name = (listed.find(&selector))
            .map_or_else(|| selector.to_string(), |space| space.name.to_string());
        let discard = assignments::discard_stopped_agent(&ctx.paths, selector, execution.clone());
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
                names.push(discarded.space.to_string());
            }
            Err(error) => (report.failed).push(ui::failure(ctx, "discard", &name, &error)),
        }
    }
    report_discarded(ctx, &report, &names)?;
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// Report the agents `discarded` of the spaces `names` lists, or that there
/// was none.
fn report_discarded(ctx: &Context, discarded: &DiscardedAgents, names: &[String]) -> Result<()> {
    ctx.show(discarded, |discarded| match names {
        [] if discarded.failed.is_empty() => {
            outln!("No stopped agents, commands or failed launches")
        }
        [] => {}
        names => outln!("Discarded the agents of {}", names.join(", ")),
    })
}
