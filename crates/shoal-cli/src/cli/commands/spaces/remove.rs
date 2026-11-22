//! Removing spaces after checking what each loses.
use anyhow::{Result, ensure};

use super::super::removal::{ensure_batch_removable, leaving};
use crate::client::output::removal::{BackgroundRemoval, Removals, Removed};
use crate::{
    cli::{args::lifecycle::RmArgs, commands::queued_agents, context::Context, output::Style, ui},
    removal::{RemovalCheck, RemovalResult, SpaceRemovalCheck, UnsavedWork},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::{self, SpaceName, SpaceSelector};

pub(in crate::cli::commands) async fn remove(ctx: &Context, args: RmArgs) -> Result<i32> {
    let RmArgs {
        space: names,
        confirmation,
        wait,
    } = args;
    let yes = confirmation.yes;
    // A number always names a space, never a queued launch.
    if let [SpaceSelector::Id(selector)] = names.as_slice()
        && let Some(code) = queued_agents::remove(ctx, selector.as_str()).await?
    {
        return Ok(code);
    }
    let selected =
        ui::select_spaces(ctx, names, true, &crate::cli::ui::Acting::Unconverted).await?;
    let checks = check_removals(ctx, selected, yes).await?;
    if !yes {
        confirm_removals(ctx, &checks)?;
    }
    match <[SpaceRemovalCheck; 1]>::try_from(checks) {
        Ok([check]) => remove_one(ctx, check, wait).await,
        Err(checks) => remove_each(ctx, checks, wait).await,
    }
}

/// Check every space `names` select, each once, before removing any, so one
/// that cannot be removed leaves the others in place.
async fn check_removals(
    ctx: &Context,
    names: Vec<SpaceSelector>,
    yes: bool,
) -> Result<Vec<SpaceRemovalCheck>> {
    let mut checks: Vec<SpaceRemovalCheck> = Vec::new();
    let mut refusals = Vec::new();
    let several = names.len() > 1;
    for name in names {
        let include_changes = ctx.interactive() && !yes;
        let checked =
            crate::client::removal::check(&ctx.paths, name.clone(), include_changes).await;
        if !several {
            checks.push(checked?);
            continue;
        }
        match checked {
            Ok(check) if checks.iter().any(|seen| seen.space.id == check.space.id) => {}
            Ok(check) => match ensure_batch_removable(ctx, &check, ("removal", "remove")).await {
                Ok(()) => checks.push(check),
                Err(error) => refusals.push(format!("{}: {error:#}", check.space.name)),
            },
            Err(error) => refusals.push(format!("{name}: {error:#}")),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot remove these spaces, so none was removed:\n  {}",
        refusals.join("\n  ")
    );
    Ok(checks)
}

async fn remove_one(ctx: &Context, check: SpaceRemovalCheck, wait: bool) -> Result<i32> {
    let removed = remove_checked(ctx, &check, None, wait).await?;
    let message = removal_message(&check.space.name, &removed);
    ctx.emit_styled(Style::Success, &message, removed)?;
    Ok(0)
}

/// Remove each checked space, those below another named one after it,
/// going on after one fails. One that a removal before it took, or takes in
/// the background, with its own is not removed again.
async fn remove_each(ctx: &Context, checks: Vec<SpaceRemovalCheck>, wait: bool) -> Result<i32> {
    let checks = ancestors_first(checks);
    // Names repeat, so what a removal took is told by ID.
    let mut taken: Vec<SpaceId> = Vec::new();
    // The spaces below a space whose removal failed, which it may have
    // taken before it failed.
    let mut maybe_taken: Vec<SpaceId> = Vec::new();
    let mut report = ui::Report::new("remove");
    for check in checks {
        let (name, id) = (&check.space.name, &check.space.id);
        if taken.contains(id) || (maybe_taken.contains(id) && !present(ctx, id).await?) {
            continue;
        }
        let below = check.children.iter().map(|child| child.id.clone());
        match remove_checked(ctx, &check, Some(name), wait).await {
            Ok(removed) => {
                taken.extend(below);
                report.done(ctx, &removal_message(name, &removed), removed);
            }
            Err(error) => {
                maybe_taken.extend(below);
                report.failed(ctx, name, &error);
            }
        }
    }
    report.finish(ctx, "", |removed, failed| Removals { removed, failed })
}

/// Whether the space `id` still exists.
async fn present(ctx: &Context, id: &SpaceId) -> Result<bool> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    Ok(spaces.find(&SpaceSelector::id(id)).is_some())
}

/// `checks` with each space after those of the others it lies below, so a
/// removal that takes it is tried first.
fn ancestors_first(checks: Vec<SpaceRemovalCheck>) -> Vec<SpaceRemovalCheck> {
    let depths: Vec<usize> = (checks.iter())
        .map(|check| {
            let id = &check.space.id;
            let takes =
                |other: &&SpaceRemovalCheck| other.children.iter().any(|child| &child.id == id);
            checks.iter().filter(takes).count()
        })
        .collect();
    let mut ordered: Vec<_> = depths.into_iter().zip(checks).collect();
    ordered.sort_by_key(|(depth, _)| *depth);
    ordered.into_iter().map(|(_, check)| check).collect()
}

/// Remove the space `check` inspected, which a removal of several reports
/// by its `name`, or without `wait` start removing it; either deletes only
/// the unsaved work it saw: more appearing meanwhile makes the daemon
/// refuse.
async fn remove_checked(
    ctx: &Context,
    check: &SpaceRemovalCheck,
    name: Option<&SpaceName>,
    wait: bool,
) -> Result<Removed> {
    let (removal, background) = match wait {
        true => (Some(finish_removal(ctx, check).await?), None),
        false => (None, Some(start_removal(ctx, check).await?)),
    };
    Ok(Removed {
        space: name.cloned(),
        removal,
        background,
        holds: check.space.holds.clone(),
        disowned: (check.worktree.as_ref()).is_some_and(|worktree| worktree.disowned),
    })
}

/// Remove the space `check` inspected, warning of a post-remove hook that
/// failed.
async fn finish_removal(ctx: &Context, check: &SpaceRemovalCheck) -> Result<RemovalResult> {
    let unsaved = UnsavedWork::confirming(check);
    let selector = SpaceSelector::id(&check.space.id);
    let removal = crate::client::removal::remove(&ctx.paths, selector, unsaved);
    let result = leaving(ctx, &check.space, removal).await?;
    if let Some(error) = &result.hook_error {
        eprintln!("warning: {error}");
    }
    Ok(result)
}

/// Start removing the space `check` inspected, which takes the spaces below
/// it the check listed.
async fn start_removal(ctx: &Context, check: &SpaceRemovalCheck) -> Result<BackgroundRemoval> {
    let unsaved = UnsavedWork::confirming(check);
    let selector = SpaceSelector::id(&check.space.id);
    let removal = crate::client::removal::start(&ctx.paths, selector, unsaved);
    leaving(ctx, &check.space, removal).await?;
    Ok(BackgroundRemoval {
        children: check
            .children
            .iter()
            .map(|child| child.name.clone())
            .collect(),
    })
}

/// Ask once before removing spaces that would lose spaces below them,
/// unsaved work or holds, naming what each loses.
fn confirm_removals(ctx: &Context, checks: &[SpaceRemovalCheck]) -> Result<()> {
    let losses: Vec<(String, Vec<String>)> = (checks.iter())
        .map(|check| (check.space.name.to_string(), removal_losses(ctx, check)))
        .collect();
    if losses.iter().all(|(_, lines)| lines.is_empty()) {
        return Ok(());
    }
    let action = match checks {
        [check] => format!("Remove {}: {}", check.space.kind(), check.space.name),
        _ => format!(
            "Remove {} spaces: {}",
            losses.len(),
            losses
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    ensure!(
        ui::confirm_losses(ctx, &action, &losses, "--yes")?,
        "removal canceled"
    );
    Ok(())
}

/// What removing the space `check` inspected loses: the spaces below it,
/// the unsaved work of its worktree, with the changed files on a terminal,
/// and its holds. Nothing in a disposable workspace counts.
fn removal_losses(ctx: &Context, check: &SpaceRemovalCheck) -> Vec<String> {
    let mut lines: Vec<String> = children_loss(check).into_iter().collect();
    let worktree = check.worktree.as_ref();
    lines.extend(worktree.and_then(disowned_loss));
    lines.extend(worktree.map(unsaved_losses).unwrap_or_default());
    if !worktree.is_some_and(|worktree| worktree.workspace.disposable) {
        lines.extend(super::super::holds::losses(&check.space.holds));
    }
    if ctx.interactive() {
        lines.extend(worktree.map(changed_files).unwrap_or_default());
    }
    lines
}

/// That removing the space `check` inspected takes the spaces below it
/// first, naming those that are not disposable; none when it takes no such
/// space.
fn children_loss(check: &SpaceRemovalCheck) -> Option<String> {
    let below: Vec<&str> = (check.children.iter())
        .filter(|child| !check.disposable_children.contains(&child.id))
        .map(|child| child.name.as_str())
        .collect();
    (!below.is_empty()).then(|| {
        format!(
            "Removes the spaces below it first, whatever their work or holds; shoal restore brings each back: {}",
            below.join(", ")
        )
    })
}

/// That Git no longer knows the worktree `check` inspected, so removal
/// reads what is left there against its branch; none for a disposable
/// workspace, whose files count for nothing.
fn disowned_loss(check: &RemovalCheck) -> Option<String> {
    (check.disowned && !check.workspace.disposable).then(|| {
        "Git no longer knows its worktree, so files left there that differ from its branch count as uncommitted; the branch stays.".into()
    })
}

/// What removing the worktree `check` inspected deletes, and whether its
/// checkpoint keeps it.
fn unsaved_losses(check: &RemovalCheck) -> Vec<String> {
    let unsaved = check.unsaved_work();
    if unsaved.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = (unsaved.iter())
        .map(|work| format!("Deletes: {work}"))
        .collect();
    lines.push(if !check.dirty || check.unsaved_files_archived {
        "shoal restore brings them back until the checkpoint expires.".into()
    } else {
        "The checkpoint cannot keep the uncommitted files (they include a repository or a changed submodule), so they cannot be restored.".into()
    });
    lines
}

/// The uncommitted changes and untracked files of the worktree `check`
/// inspected, as Git's status lists them.
fn changed_files(check: &RemovalCheck) -> Vec<String> {
    if check.changed_files.is_empty() && check.changed_files_omitted == 0 {
        return Vec::new();
    }
    let mut lines = vec!["Uncommitted changes and untracked files (Git status):".to_owned()];
    lines.extend(check.changed_files.iter().map(|file| format!("  {file}")));
    if check.changed_files_omitted > 0 {
        lines.push(format!(
            "  ... {} more entries omitted; run git status in the workspace for the full list",
            check.changed_files_omitted
        ));
    }
    lines
}

/// That the space `name` was removed, with the Git branch the removal kept
/// and the spaces below it it took, or that it is being removed, with those
/// it takes, for one that goes on in the background.
fn removal_message(name: &SpaceName, removed: &Removed) -> String {
    let (mut message, also) = match &removed.removal {
        Some(result) => match &result.branch {
            Some(branch) if !result.branch_outcome.is_deleted() => (
                format!("Removed {name}; kept Git branch {branch}"),
                "removed",
            ),
            _ => (format!("Removed {name}"), "removed"),
        },
        None => (format!("Removing {name}"), "removing"),
    };
    if !removed.children().is_empty() {
        message.push_str(&format!(
            "; also {also} {}",
            space_names::join(removed.children(), ", ")
        ));
    }
    if removed.disowned {
        message.push_str(match removed.removal {
            Some(_) => "; Git no longer knew its worktree",
            None => "; Git no longer knows its worktree",
        });
    }
    message
}
