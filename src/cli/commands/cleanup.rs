//! Removing what Shoal left behind: the spaces idle or merged cleanup would
//! remove, branches removal kept, and shared caches.
use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::lifecycle::{CleanupArgs, CleanupKind},
        client::request,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    daemon::{
        caches::Emptying,
        cleanup::{ManualCleanup, Pick, Selection},
    },
    model::KeptBranch,
    protocol::RemovalMethod,
};

use super::branches;
use crate::client::output::cleanup::Report;

pub(super) async fn run(
    ctx: &Context,
    kind: Option<CleanupKind>,
    args: CleanupArgs,
) -> Result<i32> {
    let report = match kind {
        None => everything(ctx, &args).await?,
        Some(CleanupKind::Spaces { names }) => {
            let spaces = if names.is_empty() {
                Pick::All
            } else {
                Pick::Only(names)
            };
            let selection = selection(&args, spaces, Pick::None);
            Report {
                cleanup: clean(ctx, args.dry_run, selection).await?,
                branches: Vec::new(),
            }
        }
        Some(CleanupKind::Caches) => {
            let selection = selection(&args, Pick::None, Pick::All);
            Report {
                cleanup: clean(ctx, args.dry_run, selection).await?,
                branches: Vec::new(),
            }
        }
        Some(CleanupKind::Branches { names, all }) => {
            delete_branches(ctx, &args, &names, all).await?
        }
    };
    show(ctx, &report)
}

fn selection(args: &CleanupArgs, spaces: Pick, caches: Pick) -> Selection {
    Selection {
        spaces,
        caches,
        repository: args.repo.clone(),
    }
}

async fn clean(ctx: &Context, dry_run: bool, selection: Selection) -> Result<ManualCleanup> {
    ctx.progress(
        "Cleaning up",
        request::<ManualCleanup>(&ctx.paths, RemovalMethod::Cleanup { dry_run, selection }),
    )
    .await
}

/// List every kind; on a terminal, remove what the user picks.
async fn everything(ctx: &Context, args: &CleanupArgs) -> Result<Report> {
    let listing = Report {
        cleanup: clean(ctx, true, selection(args, Pick::All, Pick::All)).await?,
        branches: branches::kept(ctx, args.repo.as_deref()).await?,
    };
    let choices = choices(&listing);
    if args.dry_run || !ctx.interactive() || choices.is_empty() {
        return Ok(listing);
    }
    let ids = ui::pick_many(ctx, "Clean up> ", choices)?;
    ensure!(!ids.is_empty(), "nothing selected");
    let picked = Picked::resolve(&ids, listing.branches);
    branches::confirm(ctx, &picked.branches, args.yes)?;
    let cleanup = if picked.spaces.is_empty() && picked.caches.is_empty() {
        ManualCleanup::default()
    } else {
        let selection = selection(args, Pick::Only(picked.spaces), Pick::Only(picked.caches));
        clean(ctx, false, selection).await?
    };
    Ok(Report {
        cleanup,
        branches: branches::delete(ctx, &picked.branches).await?,
    })
}

/// What the user picked: space names, the IDs of repositories whose caches
/// to empty, and kept branches.
struct Picked {
    spaces: Vec<String>,
    caches: Vec<String>,
    branches: Vec<KeptBranch>,
}

impl Picked {
    /// Resolve the picker's row IDs, as [`choices`] made them, against the
    /// listed kept branches.
    fn resolve(ids: &[String], kept: Vec<KeptBranch>) -> Self {
        let chosen = |prefix: &str| -> Vec<String> {
            ids.iter()
                .filter_map(|id| id.strip_prefix(prefix))
                .map(str::to_owned)
                .collect()
        };
        let branches = chosen(BRANCH);
        Self {
            spaces: chosen(SPACE),
            caches: chosen(CACHES),
            branches: kept
                .into_iter()
                .enumerate()
                .filter(|(index, _)| branches.contains(&index.to_string()))
                .map(|(_, branch)| branch)
                .collect(),
        }
    }
}

async fn delete_branches(
    ctx: &Context,
    args: &CleanupArgs,
    names: &[String],
    all: bool,
) -> Result<Report> {
    let kept = branches::kept(ctx, args.repo.as_deref()).await?;
    let cleanup = ManualCleanup {
        dry_run: args.dry_run,
        ..ManualCleanup::default()
    };
    if args.dry_run {
        let branches = if names.is_empty() {
            kept
        } else {
            branches::select_named(kept, names)?
        };
        return Ok(Report { cleanup, branches });
    }
    let selected = branches::select(ctx, kept, names, all)?;
    branches::confirm(ctx, &selected, args.yes)?;
    Ok(Report {
        cleanup,
        branches: branches::delete(ctx, &selected).await?,
    })
}

const SPACE: &str = "space:";
const BRANCH: &str = "branch:";
const CACHES: &str = "caches:";

/// The picker's rows: the spaces to remove, kept branches, and caches
/// nothing is using, each labelled with its kind. Caches go by repository ID,
/// and a repository name another listed one shares carries its ID too.
fn choices(listing: &Report) -> Vec<(String, String)> {
    let spaces = listing
        .cleanup
        .removed
        .iter()
        .map(|name| (format!("{SPACE}{name}"), format!("space      {name}")));
    let branches = branches::lines(&listing.branches)
        .into_iter()
        .enumerate()
        .map(|(index, line)| (format!("{BRANCH}{index}"), format!("branch     {line}")));
    let caches = listing.cleanup.caches.iter().filter_map(|cleanup| {
        let Emptying::Emptied { caches } = &cleanup.emptying else {
            return None;
        };
        let shared = listing
            .cleanup
            .caches
            .iter()
            .filter(|other| other.repository == cleanup.repository)
            .count()
            > 1;
        let label = if shared {
            format!("{} ({})", cleanup.repository, cleanup.repository_id)
        } else {
            cleanup.repository.clone()
        };
        Some((
            format!("{CACHES}{}", cleanup.repository_id),
            format!("caches     {label}  {}", caches.join(", ")),
        ))
    });
    spaces.chain(branches).chain(caches).collect()
}

fn show(ctx: &Context, report: &Report) -> Result<i32> {
    let cleanup = &report.cleanup;
    let failed = cleanup
        .caches
        .iter()
        .any(|caches| matches!(caches.emptying, Emptying::Failed { .. }));
    ctx.show(report, |report| {
        if cleanup.dry_run {
            show_listing(report);
        } else {
            show_outcome(report, ctx.json);
        }
        show_kept(cleanup);
        let palette = Palette::stderr(ctx.json);
        for retained in &cleanup.retained {
            eprintln!(
                "{}",
                palette.paint(
                    Style::Error,
                    format!("Retained {}: {}", retained.space, retained.error)
                )
            );
        }
        for caches in &cleanup.caches {
            if let Emptying::Failed { error } = &caches.emptying {
                eprintln!(
                    "{}",
                    palette.paint(
                        Style::Error,
                        format!("Could not empty caches of {}: {error}", caches.repository)
                    )
                );
            }
        }
    })?;
    Ok(if cleanup.retained.is_empty() && !failed {
        0
    } else {
        1
    })
}

/// What a dry run found, by kind.
fn show_listing(report: &Report) {
    let cleanup = &report.cleanup;
    let caches = |busy: bool| -> Vec<String> {
        cleanup
            .caches
            .iter()
            .filter_map(|cleanup| match &cleanup.emptying {
                Emptying::Emptied { caches } if !busy => Some((cleanup, caches)),
                Emptying::Busy { caches } if busy => Some((cleanup, caches)),
                _ => None,
            })
            .map(|(cleanup, caches)| format!("{}  {}", cleanup.repository, caches.join(", ")))
            .collect()
    };
    let groups = [
        ("Spaces to remove", cleanup.removed.clone()),
        ("Kept branches", branches::lines(&report.branches)),
        ("Caches to empty", caches(false)),
        ("Caches in use", caches(true)),
    ];
    if groups[..3].iter().all(|(_, lines)| lines.is_empty()) {
        outln!("Nothing to clean up");
    }
    for (heading, lines) in groups {
        if lines.is_empty() {
            continue;
        }
        outln!("{heading}:");
        for line in lines {
            outln!("  {line}");
        }
    }
}

/// What a cleanup removed.
fn show_outcome(report: &Report, json: bool) {
    let cleanup = &report.cleanup;
    let palette = Palette::stdout(json);
    let mut done = cleanup
        .removed
        .iter()
        .map(|name| format!("Removed {name}"))
        .chain(
            report
                .branches
                .iter()
                .map(|kept| format!("Deleted branch {} in {}", kept.branch, kept.repository)),
        )
        .chain(
            cleanup
                .caches
                .iter()
                .filter_map(|caches| match &caches.emptying {
                    Emptying::Emptied { caches: names } => Some(format!(
                        "Emptied caches of {}: {}",
                        caches.repository,
                        names.join(", ")
                    )),
                    _ => None,
                }),
        )
        .peekable();
    if done.peek().is_none() && cleanup.retained.is_empty() {
        outln!("Nothing to clean up");
    }
    for line in done {
        outln!("{}", palette.paint(Style::Success, line));
    }
    for caches in &cleanup.caches {
        if let Emptying::Busy { caches: names } = &caches.emptying {
            outln!(
                "Kept caches of {}: {} (an execution or seed build is running)",
                caches.repository,
                names.join(", ")
            );
        }
    }
}

/// The spaces cleanup kept, with why.
fn show_kept(cleanup: &ManualCleanup) {
    if cleanup.kept.is_empty() {
        return;
    }
    outln!("Kept spaces:");
    let width = cleanup
        .kept
        .iter()
        .map(|kept| kept.space.chars().count())
        .max()
        .unwrap_or(0);
    for kept in &cleanup.kept {
        outln!("  {:width$}  {}", kept.space, kept.reason);
    }
}
