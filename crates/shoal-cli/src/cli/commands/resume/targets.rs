//! What `shoal resume` acts on: workspaces with stopped work, the chats
//! Shoal saved, and the running spaces whose agents an interrupt left idle,
//! which one lookup finds by number or ID.
use anyhow::{Result, bail, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    execution::recovery::Saved,
    model::{Chat, PresentSpace, Spaces, Workspace},
};

/// The space a `shoal resume` target names.
#[derive(Debug, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, which the selection takes apart at once"
)]
pub(super) enum Target {
    /// A stopped chat, which restores at once.
    Chat(Chat),
    /// A running chat, whose agent may continue the turn an interrupt
    /// ended.
    Running(Chat),
    /// A workspace by ID, or a selector no space has, which the daemon
    /// refuses.
    Workspace(SpaceSelector),
}

/// The space `selector` names among `spaces`: a stopped or running chat or
/// a workspace.
pub(super) fn resolve(spaces: &Spaces, selector: SpaceSelector) -> Target {
    match spaces.find(&selector) {
        Some(PresentSpace::Workspace(workspace)) => {
            Target::Workspace(SpaceSelector::Id(workspace.id))
        }
        Some(PresentSpace::Chat(chat)) if chat.restore.is_some() => Target::Chat(chat),
        Some(PresentSpace::Chat(chat)) => Target::Running(chat),
        None => Target::Workspace(selector),
    }
}

/// The spaces a resume acts on.
#[derive(Debug, Default)]
pub(super) struct Selected {
    pub chats: Vec<Chat>,
    /// The running chats named, which have nothing to resume but a turn an
    /// interrupt ended.
    pub running: Vec<Chat>,
    /// By ID, or by a selector the daemon refuses; every workspace with
    /// stopped work for `None`.
    pub workspaces: Option<Vec<SpaceSelector>>,
}

impl Selected {
    /// The spaces `targets` names, each once; every stopped chat and every
    /// workspace for `--all`; otherwise the current workspace when it has
    /// something to resume, else those picked among the workspaces and saved
    /// chats, or all of them when nothing is resumable anywhere.
    pub async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Self> {
        let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
        let saved = saved_chats(&spaces);
        if targets.all {
            return Ok(Self::every(saved));
        }
        match ui::select_stopped_spaces(ctx, targets.space, &saved).await? {
            Some(selectors) => Ok(Self::resolved(&spaces, selectors)),
            None => Ok(Self::every(saved)),
        }
    }

    /// The stopped work and last sessions of the selected workspaces, or of
    /// every workspace with any, as [`saved_work`] keeps them for `discard`.
    pub async fn stopped(&self, ctx: &Context, discard: bool) -> Result<Vec<(Workspace, Saved)>> {
        match &self.workspaces {
            None => stopped_work(ctx, discard).await,
            Some(workspaces) => stopped_in(ctx, workspaces, discard).await,
        }
    }

    /// The spaces `selectors` name among `spaces`, each once.
    fn resolved(spaces: &Spaces, selectors: Vec<SpaceSelector>) -> Self {
        let mut selected = Self {
            chats: Vec::new(),
            running: Vec::new(),
            workspaces: Some(Vec::new()),
        };
        for selector in selectors {
            selected.add(resolve(spaces, selector));
        }
        selected
    }

    /// The `saved` chats and every workspace.
    fn every(saved: Vec<Chat>) -> Self {
        Self {
            chats: saved,
            running: Vec::new(),
            workspaces: None,
        }
    }

    /// Whether nothing is selected, not even every workspace.
    pub fn is_empty(&self) -> bool {
        self.chats.is_empty()
            && self.running.is_empty()
            && self.workspaces.as_ref().is_some_and(Vec::is_empty)
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
            Target::Chat(chat) if !self.chats.contains(&chat) => self.chats.push(chat),
            Target::Chat(_) => {}
            Target::Running(chat) if !self.running.contains(&chat) => self.running.push(chat),
            Target::Running(_) => {}
            Target::Workspace(workspace) => {
                let workspaces = self.workspaces.get_or_insert_default();
                if !workspaces.contains(&workspace) {
                    workspaces.push(workspace);
                }
            }
        }
    }
}

/// The stopped chats among `spaces`.
fn saved_chats(spaces: &Spaces) -> Vec<Chat> {
    spaces
        .chats
        .iter()
        .filter(|chat| chat.restore.is_some())
        .cloned()
        .collect()
}

/// The stopped work and last sessions of the workspaces `selectors` name,
/// as [`saved_work`] keeps them for `discard`, refusing one with nothing to
/// resume.
async fn stopped_in(
    ctx: &Context,
    selectors: &[SpaceSelector],
    discard: bool,
) -> Result<Vec<(Workspace, Saved)>> {
    let mut stopped = Vec::new();
    for selector in selectors {
        let workspace = crate::client::workspaces::inspect(&ctx.paths, selector.clone())
            .await?
            .workspace;
        let saved = saved_work(ctx, &workspace, discard)?;
        ensure!(
            !saved.is_empty(),
            "{} has no stopped agents or commands to resume",
            workspace.name
        );
        stopped.push((workspace, saved));
    }
    Ok(stopped)
}

/// The workspaces with stopped work or last sessions, as [`saved_work`]
/// keeps them for `discard`. A done assignment needs no agent, so its
/// workspace is included only to `discard`.
async fn stopped_work(ctx: &Context, discard: bool) -> Result<Vec<(Workspace, Saved)>> {
    let mut stopped = Vec::new();
    for workspace in crate::client::workspaces::list(&ctx.paths).await? {
        if workspace.done && !discard {
            continue;
        }
        let saved = saved_work(ctx, &workspace, discard)?;
        if !saved.is_empty() {
            stopped.push((workspace, saved));
        }
    }
    Ok(stopped)
}

/// `workspace`'s stopped work and last sessions: with the agents waiting in
/// their wrappers, which a resume starts there, except to `discard`, which
/// leaves them to their wrappers.
fn saved_work(ctx: &Context, workspace: &Workspace, discard: bool) -> Result<Saved> {
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if discard {
        saved.retain_unattended();
    }
    Ok(saved)
}

#[cfg(test)]
mod tests;
