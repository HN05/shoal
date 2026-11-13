//! What `shoal resume` acts on: workspaces with stopped work, the chats
//! Shoal saved, and the running spaces whose agents an interrupt left idle,
//! which one lookup finds by number or ID.
use anyhow::{Result, bail, ensure};

use super::setups::FailedSetup;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    execution::recovery::Saved,
    model::{Chat, PresentSpace, Spaces},
};

/// The space a `shoal resume` target names.
#[derive(Debug, PartialEq)]
pub(super) enum Target {
    /// A running chat, whose agent may continue the turn an interrupt
    /// ended.
    Running(Box<Chat>),
    /// A space by ID, a workspace or a stopped chat, or a selector no space
    /// has, which the daemon refuses.
    Space(SpaceSelector),
}

/// The space `selector` names among `spaces`: a running chat or another
/// space.
pub(super) fn resolve(spaces: &Spaces, selector: SpaceSelector) -> Target {
    match spaces.find(&selector) {
        Some(PresentSpace::Chat(chat)) if chat.running => Target::Running(Box::new(chat)),
        Some(space) => Target::Space(SpaceSelector::id(space.id())),
        None => Target::Space(selector),
    }
}

/// The spaces a resume acts on.
#[derive(Debug, Default)]
pub(super) struct Selected {
    /// The running chats named, which have nothing to resume but a turn an
    /// interrupt ended.
    pub running: Vec<Chat>,
    /// By ID, or by a selector the daemon refuses; every space with stopped
    /// work for `None`.
    pub spaces: Option<Vec<SpaceSelector>>,
    /// The workspaces named whose setup failed, which resume by retrying
    /// it, once [`super::setups::take_failed`] takes them from the others.
    pub setups: Vec<FailedSetup>,
}

impl Selected {
    /// The spaces `targets` names, each once; every space for `--all`;
    /// otherwise the current workspace when it has something to resume,
    /// else those picked among the workspaces and stopped chats, or all of
    /// them when nothing is resumable anywhere.
    pub async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Self> {
        let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
        if targets.all {
            return Ok(Self::default());
        }
        let stopped = stopped_chats(&spaces);
        match ui::select_stopped_spaces(ctx, targets.space, &stopped).await? {
            Some(selectors) => Ok(Self::resolved(&spaces, selectors)),
            None => Ok(Self::default()),
        }
    }

    /// The stopped work and last sessions of the selected spaces, or of
    /// every space with any, as [`saved_work`] keeps them for `forget`.
    pub async fn stopped(&self, ctx: &Context, forget: bool) -> Result<Vec<(PresentSpace, Saved)>> {
        match &self.spaces {
            None => stopped_work(ctx, forget).await,
            Some(spaces) => stopped_in(ctx, spaces, forget).await,
        }
    }

    /// The spaces `selectors` name among `spaces`, each once.
    fn resolved(spaces: &Spaces, selectors: Vec<SpaceSelector>) -> Self {
        let mut selected = Self {
            spaces: Some(Vec::new()),
            ..Self::default()
        };
        for selector in selectors {
            selected.add(resolve(spaces, selector));
        }
        selected
    }

    /// Refuse the running chats selected, which have nothing to resume.
    pub fn refuse_running(&self) -> Result<()> {
        if let Some(chat) = self.running.first() {
            bail!("chat {} is running; nothing to resume", chat.name);
        }
        Ok(())
    }

    /// Add `target` unless it is already selected.
    fn add(&mut self, target: Target) {
        match target {
            Target::Running(chat) if !self.running.contains(&chat) => self.running.push(*chat),
            Target::Running(_) => {}
            Target::Space(space) => {
                let spaces = self.spaces.get_or_insert_default();
                if !spaces.contains(&space) {
                    spaces.push(space);
                }
            }
        }
    }
}

/// The stopped chats among `spaces`, which a picker offers beside the
/// workspaces.
fn stopped_chats(spaces: &Spaces) -> Vec<Chat> {
    spaces
        .chats
        .iter()
        .filter(|chat| !chat.running)
        .cloned()
        .collect()
}

/// The stopped work and last sessions of the spaces `selectors` name, as
/// [`saved_work`] keeps them for `forget`, refusing one with nothing to
/// resume.
async fn stopped_in(
    ctx: &Context,
    selectors: &[SpaceSelector],
    forget: bool,
) -> Result<Vec<(PresentSpace, Saved)>> {
    let mut stopped = Vec::new();
    for selector in selectors {
        let space = super::present_space(ctx, selector).await?;
        let saved = saved_work(ctx, &space, forget).await?;
        ensure!(
            !saved.is_empty(),
            "{} has no stopped agents or commands to resume",
            space.name()
        );
        stopped.push((space, saved));
    }
    Ok(stopped)
}

/// The spaces with stopped work or last sessions, as [`saved_work`] keeps
/// them for `forget`. A done assignment needs no agent, so its space is
/// included only to `forget`.
async fn stopped_work(ctx: &Context, forget: bool) -> Result<Vec<(PresentSpace, Saved)>> {
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let all = (spaces.workspaces.into_iter().map(PresentSpace::Workspace))
        .chain(spaces.chats.into_iter().map(PresentSpace::Chat));
    let mut stopped = Vec::new();
    for space in all {
        if space.done() && !forget {
            continue;
        }
        let saved = saved_work(ctx, &space, forget).await?;
        if !saved.is_empty() {
            stopped.push((space, saved));
        }
    }
    Ok(stopped)
}

/// The stopped work and last sessions of `space`: with the agents waiting
/// in their wrappers, which a resume starts there, except to `forget`,
/// which leaves them to their wrappers.
async fn saved_work(ctx: &Context, space: &PresentSpace, forget: bool) -> Result<Saved> {
    let mut saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(space.id())).await?;
    if forget {
        saved.retain_unattended();
    }
    Ok(saved)
}

#[cfg(test)]
mod tests;
