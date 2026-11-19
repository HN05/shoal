//! `shoal branch ls`: the branches on the forge of the space's repository,
//! or another one, with what decides whether each can go.
use anyhow::Result;

use crate::cli::{
    args::changes::TargetRepo, commands::items::select_item_target, context::Context,
};
use shoal_protocol::{branches::ListedBranch, space_names::SpaceSelector};

/// Which branches `shoal branch ls` lists, by whether the default branch
/// has their changes.
pub(super) struct MergedFilter {
    pub merged: bool,
    pub unmerged: bool,
}

impl MergedFilter {
    /// The merged state the daemon keeps; none keeps every branch.
    fn state(&self) -> Option<bool> {
        match (self.merged, self.unmerged) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
    }
}

pub(super) async fn list(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: TargetRepo,
    filter: MergedFilter,
) -> Result<i32> {
    let target = select_item_target(ctx, space, repo.name, None).await?;
    let listing = crate::client::branches::list(&ctx.paths, target, filter.state());
    let listed = ctx.progress("Reading branches", listing).await?;
    ctx.show(&listed, |listed| render(listed))?;
    Ok(0)
}

/// One aligned line per branch: its name, commit, the commit's day, whether
/// the default branch has its changes, then what else keeps it.
fn render(listed: &[ListedBranch]) {
    if listed.is_empty() {
        outln!("No branches");
        return;
    }
    let width = (listed.iter())
        .map(|listed| listed.branch.name.as_str().chars().count())
        .max()
        .unwrap_or(0);
    for listed in listed {
        let branch = &listed.branch;
        let day = (branch.committed_at.as_deref())
            .map(|at| at.get(..10).unwrap_or(at))
            .unwrap_or("");
        let line = format!(
            "{:width$}  {}  {day:10}  {:8}  {}",
            branch.name,
            branch.head.short(),
            merged(listed.merged),
            notes(listed).join("  "),
        );
        outln!("{}", line.trim_end());
    }
}

fn merged(merged: Option<bool>) -> &'static str {
    match merged {
        Some(true) => "merged",
        Some(false) => "unmerged",
        None => "unknown",
    }
}

/// What else decides whether the branch can go: being the default or
/// protected, the open PRs whose head it is, and the space whose branch it
/// is with where its work stands.
fn notes(listed: &ListedBranch) -> Vec<String> {
    let mut notes = Vec::new();
    if listed.default {
        notes.push("default".to_owned());
    }
    if listed.branch.protected {
        notes.push("protected".to_owned());
    }
    notes.extend(listed.open_prs.iter().map(|number| format!("PR #{number}")));
    if let Some(space) = &listed.space {
        let (number, name) = (space.number, &space.name);
        notes.push(format!("space {number} {name} ({})", space.state.as_str()));
    }
    notes
}
