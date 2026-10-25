//! Branches Shoal kept when it removed their workspaces, because only they
//! have some commits, and deleting them on request.
use anyhow::{Result, bail, ensure};

use crate::{
    cli::{client::request, context::Context, ui},
    model::{BranchTarget, KeptBranch},
    protocol::Method,
    time::local_seconds,
};

/// The kept branches, only those of `repo` when it names one by name or ID.
pub(super) async fn kept(ctx: &Context, repo: Option<&str>) -> Result<Vec<KeptBranch>> {
    Ok(request::<Vec<KeptBranch>>(&ctx.paths, Method::KeptBranches)
        .await?
        .into_iter()
        .filter(|branch| {
            repo.is_none_or(|repo| branch.repository == repo || branch.repository_id == repo)
        })
        .collect())
}

/// The kept branches to delete: every one with `all`, the named ones, or
/// those chosen in a picker.
pub(super) fn select(
    ctx: &Context,
    kept: Vec<KeptBranch>,
    names: &[String],
    all: bool,
) -> Result<Vec<KeptBranch>> {
    ensure!(!kept.is_empty(), "no kept branches");
    if all {
        Ok(kept)
    } else if !names.is_empty() {
        select_named(kept, names)
    } else {
        pick(ctx, kept)
    }
}

/// Ask before deleting `selected` and the commits only they have, unless `yes`.
pub(super) fn confirm(ctx: &Context, selected: &[KeptBranch], yes: bool) -> Result<()> {
    if yes || selected.is_empty() {
        return Ok(());
    }
    let mut action = String::from("Delete kept branches and the commits only they have:");
    for line in lines(selected) {
        action.push_str("\n  ");
        action.push_str(&line);
    }
    ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    Ok(())
}

/// Delete `selected`, each only while it is at the commit the user saw.
pub(super) async fn delete(ctx: &Context, selected: &[KeptBranch]) -> Result<Vec<KeptBranch>> {
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let targets = selected
        .iter()
        .map(|branch| BranchTarget {
            repository_id: branch.repository_id.clone(),
            branch: branch.branch.clone(),
            head: branch.head.clone(),
        })
        .collect();
    request(&ctx.paths, Method::DeleteKeptBranches { branches: targets }).await
}

/// The kept branches `names` name, each in exactly one repository.
pub(super) fn select_named(kept: Vec<KeptBranch>, names: &[String]) -> Result<Vec<KeptBranch>> {
    let mut selected = Vec::new();
    for name in names {
        let mut matches = kept.iter().filter(|branch| &branch.branch == name);
        match (matches.next(), matches.next()) {
            (Some(branch), None) => selected.push(branch.clone()),
            (Some(_), Some(_)) => {
                bail!("{name} is kept in several repositories; choose one with --repo")
            }
            (None, _) => {
                bail!("{name} is not a kept branch; shoal cleanup branches --dry-run lists them")
            }
        }
    }
    Ok(selected)
}

fn pick(ctx: &Context, kept: Vec<KeptBranch>) -> Result<Vec<KeptBranch>> {
    ensure!(
        ctx.interactive(),
        "name the branches to delete or pass --all; shoal cleanup branches --dry-run lists them"
    );
    let entries = (0..kept.len())
        .map(|index| index.to_string())
        .zip(lines(&kept))
        .collect();
    let picked = ui::pick_many(ctx, "Delete branches> ", entries)?;
    ensure!(!picked.is_empty(), "no branches selected");
    Ok(kept
        .into_iter()
        .enumerate()
        .filter(|(index, _)| picked.contains(&index.to_string()))
        .map(|(_, branch)| branch)
        .collect())
}

/// One aligned line per kept branch: its repository and name, how many
/// commits only it has, and its last commit.
pub(super) fn lines(kept: &[KeptBranch]) -> Vec<String> {
    let width = |field: fn(&KeptBranch) -> &str| {
        kept.iter()
            .map(|branch| field(branch).chars().count())
            .max()
            .unwrap_or(0)
    };
    let repository_width = width(|branch| &branch.repository);
    let branch_width = width(|branch| &branch.branch);
    kept.iter()
        .map(|branch| {
            let commits = match branch.unpushed_commits {
                1 => "1 unpushed commit".to_owned(),
                count => format!("{count} unpushed commits"),
            };
            format!(
                "{:repository_width$}  {:branch_width$}  {commits}  last commit {}: {}",
                branch.repository,
                branch.branch,
                local_seconds(branch.committed_at as i64),
                branch.subject,
            )
        })
        .collect()
}
