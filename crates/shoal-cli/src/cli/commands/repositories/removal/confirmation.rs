//! What `shoal repo rm` asks the user to confirm: each machine's
//! repository, with the spaces its removal deletes there and the work of
//! their own they lose.
use anyhow::Result;
use shoal_protocol::model::{RepositoryRemovalPreview, SpaceWork, WorktreeWork};

use super::{Target, machine_name, request};
use crate::cli::context::Context;

/// The removal of `targets`, each machine's spaces and their work as the
/// machine reads them now.
pub(super) async fn describe(ctx: &Context, targets: &[Target]) -> Result<String> {
    let mut blocks = Vec::new();
    for target in targets {
        let (selector, machine) = request(target);
        let preview = crate::client::repositories::preview_removal(&ctx.paths, selector, machine);
        let preview = ctx
            .progress("Reading what removal deletes", preview)
            .await?;
        blocks.push(block(&preview, machine_name(target).as_deref()));
    }
    blocks.push(
        "Deletes: checkout, all Shoal workspaces and their resources\nWork:    uncommitted and unpushed changes are permanently lost"
            .to_owned(),
    );
    Ok(blocks.join("\n\n"))
}

/// The removal of `targets` in one line, for a refusal that asks for
/// `--yes`.
pub(super) fn briefly(targets: &[Target]) -> String {
    let named: Vec<String> = (targets.iter())
        .map(|target| match target {
            Target::Here(local) => local.display_name().to_owned(),
            Target::Elsewhere(other) => format!("{} on {}", other.name(), other.machine),
        })
        .collect();
    format!("Delete repository {}", named.join(", "))
}

/// What `preview` deletes on the machine `machine` names, none for this
/// one.
fn block(preview: &RepositoryRemovalPreview, machine: Option<&str>) -> String {
    let repository = &preview.repository;
    let on = machine
        .map(|machine| format!(" on {machine}"))
        .unwrap_or_default();
    let mut lines = vec![
        format!("Delete repository: {}{on}", repository.display_name()),
        format!("Source: {}", repository.source),
        format!("Checkout: {}", repository.path.display()),
    ];
    for (index, space) in preview.spaces.iter().enumerate() {
        let label = if index == 0 { "Spaces:" } else { "" };
        lines.push(format!("{label:<8}{}", space_line(space)));
    }
    lines.join("\n")
}

/// A space that removal deletes, with the work of its own it loses.
fn space_line(space: &SpaceWork) -> String {
    let named = format!("{} {}", space.number, space.space.name);
    let lost = match (space.space.kind().has_worktree(), space.work) {
        (false, _) => Vec::new(),
        (true, None) => vec!["work unknown: its worktree cannot be read".to_owned()],
        (true, Some(work)) => lost(work),
    };
    match lost.is_empty() {
        true => named,
        false => format!("{named}: {}", lost.join(", ")),
    }
}

/// What `work` loses, one entry per kind.
fn lost(work: WorktreeWork) -> Vec<String> {
    let mut lost = Vec::new();
    if work.dirty {
        lost.push("uncommitted files".to_owned());
    }
    match work.unpushed_commits {
        0 => {}
        1 => lost.push("1 unpushed commit".to_owned()),
        count => lost.push(format!("{count} unpushed commits")),
    }
    lost
}

#[cfg(test)]
mod tests;
