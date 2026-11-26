//! Opening removed spaces again from their removal records.
use crate::cli::commands::resume::reports::QueuedExt;
use shoal_workers::{hosting, reopening};

use anyhow::Result;

use crate::cli::commands::resume::reports::Reporting;
use crate::client::output::{restore::Reopened, resume::Queued};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        shell,
    },
    model::{Head, RemovedSpace, RestoredSpace},
};
use shoal_protocol::space_names::SpaceSelector;

/// In the order given, so a workspace opens before a chat in its worktree. An
/// error once one fails names those opened before it.
pub(in crate::cli::commands) async fn reopen_each(
    ctx: &Context,
    removed: &[RemovedSpace],
) -> Result<Vec<Reopened>> {
    let mut reopened = Vec::new();
    for space in removed {
        let opened = reopen(ctx, space).await;
        reopened.push(opened.map_err(|error| Reopening::of(&reopened).explain(error))?);
    }
    if let [opened] = reopened.as_slice()
        && let Some(workspace) = opened.space.worktree()
        && hosting::follows_caller(ctx.caller())
    {
        shell::navigate(ctx, &workspace.path)?;
    }
    Ok(reopened)
}

async fn reopen(ctx: &Context, removed: &RemovedSpace) -> Result<Reopened> {
    let restore = crate::client::removal::restore(&ctx.paths, SpaceSelector::id(&removed.space_id));
    let restored = ctx.progress("Restoring", restore).await?;
    let opened = reopening::prepare_set_up(&ctx.paths, ctx, restored).await?;
    reopening::run_post_setup(&ctx.paths, ctx, &opened).await?;
    Ok(opened)
}

/// An error that ends a restore afterwards names these, since it reports them
/// no other way.
pub(in crate::cli::commands) struct Reopening(Option<String>);

impl Reopening {
    pub(in crate::cli::commands) fn of(reopened: &[Reopened]) -> Self {
        let names = reopened
            .iter()
            .map(|opened| format!("{} as {}", opened.space.name, opened.space.number))
            .collect::<Vec<_>>();
        Self((!names.is_empty()).then(|| names.join(", ")))
    }

    pub(in crate::cli::commands) fn explain(&self, error: anyhow::Error) -> anyhow::Error {
        match &self.0 {
            Some(names) => error.context(format!("reopened {names}, then stopped")),
            None => error,
        }
    }
}

/// A workspace becomes the directory of a shell that follows the caller.
pub(in crate::cli::commands) async fn finish_reopen(
    ctx: &Context,
    restored: RestoredSpace,
    action: &str,
    restore: bool,
) -> Result<i32> {
    let Some(mut reopened) = reopening::prepare_restored(&ctx.paths, ctx, restored).await? else {
        return Ok(1);
    };
    // The agents start in a workspace whose post-setup hook succeeded.
    if restore {
        reopening::run_post_setup(&ctx.paths, ctx, &reopened).await?;
        reopening::queue_agents(&ctx.paths, ctx, &mut reopened).await;
    }
    ctx.emit(&reopened.message(ctx, action), &reopened)?;
    if let Some(workspace) = reopened.space.worktree()
        && hosting::follows_caller(ctx.caller())
    {
        shell::navigate(ctx, &workspace.path)?;
    }
    if !restore {
        reopening::run_post_setup(&ctx.paths, ctx, &reopened).await?;
    }
    Ok(0)
}

/// Operations on the client type [`Reopened`] that only the `shoal` command uses.
pub(crate) trait ReopenedExt {
    fn message(&self, ctx: &Context, action: &str) -> String;
}

impl ReopenedExt for Reopened {
    fn message(&self, ctx: &Context, action: &str) -> String {
        let Self { space, removed, .. } = self;
        let checkpoint = removed.workspace().map(|kept| &kept.checkpoint);
        let files = if checkpoint.is_some_and(|checkpoint| checkpoint.commit != checkpoint.head) {
            "; unsaved files restored"
        } else if checkpoint.is_some_and(|checkpoint| checkpoint.files_omitted) {
            "; unsaved files were not kept"
        } else {
            ""
        };
        let agents = match reopening::saved_executions(removed).len() {
            _ if self.queued.is_some() => self
                .queued
                .as_ref()
                .and_then(Queued::summary)
                .map(|summary| format!("; {summary}"))
                .unwrap_or_default(),
            _ if !self.agents_saved => String::new(),
            0 => String::new(),
            1 => format!("; shoal resume {} resumes its stopped agent", space.number),
            _ => format!("; shoal resume {} resumes its stopped agents", space.number),
        };
        let location = match space.to_workspace() {
            Some(workspace) => super::super::preparation::location_phrase(&workspace),
            None => format!("as chat {}", space.number),
        };
        format!(
            "{action} {} {location}{files}{agents}",
            Palette::stdout(ctx.json).paint(Style::Heading, &space.name),
        )
    }
}

/// A command tells of the agents it restores as `shoal resume` does, and
/// of each space a package carries as it opens it.
impl reopening::Reopening for Context {
    fn resuming(&self) -> impl shoal_workers::resume::Resuming + '_ {
        Reporting(self)
    }

    fn carried(&self, reopened: &Reopened) {
        if !self.json {
            outln!("{}", reopened.message(self, "Imported"));
        }
    }
}

pub(super) fn removed_lines(removed: &[RemovedSpace]) -> Vec<String> {
    let width = removed
        .iter()
        .map(|space| shoal_text::width(space.name.as_str()))
        .max()
        .unwrap_or(0);
    removed
        .iter()
        .map(|space| {
            let (branch, checkpoint) = match space.workspace() {
                Some(kept) => (
                    match &kept.head {
                        Head::Branch(branch) => format!("  {branch}"),
                        Head::Commit(at) => format!("  at {}", at.label()),
                    },
                    format!(", checkpoint {}", kept.checkpoint.number),
                ),
                None => Default::default(),
            };
            format!(
                "{}{branch}  removed {} ({}){checkpoint}, restorable until {}",
                shoal_text::pad(space.name.as_str(), width),
                space.removed_at.local_seconds(),
                space.cause,
                space.expires_at.local_seconds(),
            )
        })
        .collect()
}
