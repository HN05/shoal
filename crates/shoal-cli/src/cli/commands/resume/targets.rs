//! What `shoal resume` acts on: the spaces named, workspaces and chats
//! alike, or every space with a stopped agent for `--all`; without a space,
//! the current workspace when its agent can resume, else those picked among
//! the spaces whose agent can resume, or else a list of them.
use anyhow::Result;

use crate::client::output::resume::{Resumable, ResumableSpace};
use crate::{
    cli::{args::SpaceTargets, commands::queued_agents, context::Context, ui},
    model::Spaces,
    state::QueueState,
};
use shoal_protocol::space_names::SpaceSelector;

/// The spaces a resume acts on.
#[derive(Debug, Default)]
pub(in crate::cli::commands) struct Selected {
    /// By ID, or by a selector the daemon refuses; every space with
    /// something to resume for `None`.
    pub spaces: Option<Vec<SpaceSelector>>,
}

impl Selected {
    /// The spaces `targets` names, each once; every space with a stopped
    /// agent for `--all`; otherwise the current workspace when its agent
    /// can resume, else those picked among the spaces whose agent can
    /// resume. `None` once it listed those, without a terminal or anything
    /// to pick.
    pub async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Option<Self>> {
        if targets.all {
            return Ok(Some(Self::default()));
        }
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        if !targets.space.is_empty() {
            let named = ui::named_spaces(ctx, targets.space).await?;
            return Ok(Some(Self::resolved(&spaces, named)));
        }
        let resumable = resumable(ctx, &spaces).await?;
        match ui::select_resumable(ctx, &resumable).await? {
            Some(picked) => Ok(Some(Self::resolved(&spaces, picked))),
            None => {
                let resumable = Resumable { spaces: resumable };
                ctx.show(&resumable, print_resumable)?;
                Ok(None)
            }
        }
    }

    /// The spaces `selectors` name among `spaces`, by ID and each once; a
    /// selector no space has stays as given, for the daemon to refuse.
    fn resolved(spaces: &Spaces, selectors: Vec<SpaceSelector>) -> Self {
        let mut resolved = Vec::new();
        for selector in selectors {
            let selector = match spaces.find(&selector) {
                Some(space) => SpaceSelector::id(&space.id),
                None => selector,
            };
            if !resolved.contains(&selector) {
                resolved.push(selector);
            }
        }
        Self {
            spaces: Some(resolved),
        }
    }
}

/// The spaces this process's scope reaches below its own whose agent can
/// resume, with what of it resumes.
async fn in_reach(ctx: &Context) -> Result<Vec<ResumableSpace>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let scoping = crate::scoping::Scoping::inherited(&ctx.paths.state);
    resumable(ctx, &Spaces(scoping.reached_below(spaces))).await
}

/// The spaces in reach that `--all` resumes, by ID: those that saved
/// stopped work or last sessions, since a failed launch queues again only
/// for its space named or picked, and a done space's agent resumes only
/// when its space is named.
pub(super) async fn resumable_in_reach(ctx: &Context) -> Result<Vec<SpaceSelector>> {
    Ok((in_reach(ctx).await?.into_iter())
        .filter(|resumable| resumable.saved.is_some() && !resumable.space.done())
        .map(|resumable| SpaceSelector::id(&resumable.space.id))
        .collect())
}

/// The spaces in reach whose agents `--all` discards, by ID: every one with
/// saved work or a failed launch, done or not.
pub(in crate::cli::commands) async fn discardable_in_reach(
    ctx: &Context,
) -> Result<Vec<SpaceSelector>> {
    Ok((in_reach(ctx).await?.into_iter())
        .map(|resumable| SpaceSelector::id(&resumable.space.id))
        .collect())
}

/// The spaces among `spaces` whose agent can resume: those that saved a
/// stopped agent, last session or command, and those whose launch failed,
/// unfinished work first.
async fn resumable(ctx: &Context, spaces: &Spaces) -> Result<Vec<ResumableSpace>> {
    let mut summaries = ui::saved_summaries(&ctx.paths).await;
    let entries = queued_agents::entries(ctx).await?;
    let mut resumable: Vec<_> = (spaces.iter())
        .filter_map(|space| {
            let saved = summaries.remove(&space.id);
            let failed_launch = (entries.iter())
                .find(|entry| entry.state == QueueState::Failed && entry.space() == Some(&space.id))
                .cloned();
            (saved.is_some() || failed_launch.is_some()).then(|| ResumableSpace {
                space: space.clone(),
                saved,
                failed_launch,
            })
        })
        .collect();
    resumable.sort_by_key(|space| !space.unfinished());
    Ok(resumable)
}

/// Print each space of `resumable` led by its number, which selects it,
/// with what of it resumes.
fn print_resumable(resumable: &Resumable) {
    if resumable.spaces.is_empty() {
        outln!("No agents to resume");
    }
    for space in &resumable.spaces {
        let (what, _) = ui::resumable(space);
        outln!("  {}  {}  {what}", space.space.number, space.space.name);
    }
}

#[cfg(test)]
mod tests;
