//! Branches Shoal kept when it removed their workspaces, because only they
//! have some commits, and deleting them on request.
use anyhow::{Result, bail, ensure};

use crate::{
    cli::{BranchCommand, client::request, context::Context, ui},
    model::{BranchTarget, KeptBranch},
    protocol::Method,
    time::local_seconds,
};

/// `shoal branch`: list kept branches, or delete some of them.
pub(super) async fn run(ctx: &Context, command: Option<BranchCommand>) -> Result<i32> {
    match command.unwrap_or(BranchCommand::Ls) {
        BranchCommand::Ls => list(ctx).await,
        BranchCommand::Rm {
            branches,
            all,
            repo,
            confirmation,
        } => remove(ctx, branches, all, repo, confirmation.yes).await,
    }
}

async fn list(ctx: &Context) -> Result<i32> {
    let kept = request::<Vec<KeptBranch>>(&ctx.paths, Method::KeptBranches).await?;
    ctx.show(&kept, |kept| {
        if kept.is_empty() {
            println!("No kept branches");
        }
        for line in lines(kept) {
            println!("{line}");
        }
    })?;
    Ok(0)
}

async fn remove(
    ctx: &Context,
    names: Vec<String>,
    all: bool,
    repo: Option<String>,
    yes: bool,
) -> Result<i32> {
    let kept: Vec<KeptBranch> = request::<Vec<KeptBranch>>(&ctx.paths, Method::KeptBranches)
        .await?
        .into_iter()
        .filter(|branch| {
            repo.as_deref()
                .is_none_or(|repo| branch.repository == repo || branch.repository_id == repo)
        })
        .collect();
    ensure!(!kept.is_empty(), "no kept branches");
    let selected = if all {
        kept
    } else if !names.is_empty() {
        select_named(kept, &names)?
    } else {
        pick(ctx, kept)?
    };
    if !yes {
        let mut action = String::from("Delete kept branches and the commits only they have:");
        for line in lines(&selected) {
            action.push_str("\n  ");
            action.push_str(&line);
        }
        ensure!(ui::confirm(ctx, &action, "--yes")?, "deletion canceled");
    }
    let targets = selected
        .iter()
        .map(|branch| BranchTarget {
            repository_id: branch.repository_id.clone(),
            branch: branch.branch.clone(),
        })
        .collect();
    let deleted =
        request::<Vec<KeptBranch>>(&ctx.paths, Method::DeleteKeptBranches { branches: targets })
            .await?;
    let message = match deleted.as_slice() {
        [branch] => format!("Deleted branch {}", branch.branch),
        _ => format!("Deleted {} branches", deleted.len()),
    };
    ctx.emit(&message, &deleted)?;
    Ok(0)
}

/// The kept branches `names` name, each in exactly one repository.
fn select_named(kept: Vec<KeptBranch>, names: &[String]) -> Result<Vec<KeptBranch>> {
    let mut selected = Vec::new();
    for name in names {
        let mut matches = kept.iter().filter(|branch| &branch.branch == name);
        match (matches.next(), matches.next()) {
            (Some(branch), None) => selected.push(branch.clone()),
            (Some(_), Some(_)) => {
                bail!("{name} is kept in several repositories; choose one with --repo")
            }
            (None, _) => bail!("{name} is not a kept branch; shoal branch lists them"),
        }
    }
    Ok(selected)
}

fn pick(ctx: &Context, kept: Vec<KeptBranch>) -> Result<Vec<KeptBranch>> {
    ensure!(
        ctx.interactive(),
        "name the branches to delete or pass --all; shoal branch lists them"
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
fn lines(kept: &[KeptBranch]) -> Vec<String> {
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
