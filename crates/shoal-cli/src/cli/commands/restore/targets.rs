//! What `shoal restore` acts on: workspaces with stopped work, the chats
//! Shoal saved, the running spaces whose agents an interrupt left idle and
//! removed workspaces, which one lookup finds by number or ID; without a
//! space, the current one or those picked, or else a list of them all.
use anyhow::{Result, bail};

use super::agents::setups::FailedSetup;
use crate::client::output::resume::{Restorable, StoppedWorkspace};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    model::{Chat, PresentSpace, RemovedWorkspace, Spaces},
};

/// The space a `shoal restore` target names.
#[derive(Debug, PartialEq)]
pub(super) enum Target {
    /// A running chat, whose agent may continue the turn an interrupt
    /// ended.
    Running(Box<Chat>),
    /// A space by ID, a workspace or a stopped chat, or a selector no space
    /// has, which the daemon refuses.
    Space(SpaceSelector),
    /// A removed workspace by its ID, which a restore opens again.
    Removed(SpaceSelector),
}

/// The space `selector` names among `spaces` and the `removed` workspaces:
/// a running chat, another space or a removed workspace.
pub(super) fn resolve(
    spaces: &Spaces,
    removed: &[RemovedWorkspace],
    selector: SpaceSelector,
) -> Target {
    match spaces.find(&selector) {
        Some(PresentSpace::Chat(chat)) if chat.running => Target::Running(Box::new(chat)),
        Some(space) => Target::Space(SpaceSelector::id(space.id())),
        None if removed
            .iter()
            .any(|workspace| selector.given_id() == Some(&workspace.workspace_id)) =>
        {
            Target::Removed(selector)
        }
        None => Target::Space(selector),
    }
}

/// The spaces a restore acts on.
#[derive(Debug, Default)]
pub(super) struct Selected {
    /// The running chats named, which have nothing to restore but a turn an
    /// interrupt ended.
    pub running: Vec<Chat>,
    /// By ID, or by a selector the daemon refuses; every space with stopped
    /// work for `None`.
    pub spaces: Option<Vec<SpaceSelector>>,
    /// The workspaces named whose setup failed, which restore by retrying
    /// it, once [`super::agents::setups::take_failed`] takes them from the
    /// others.
    pub setups: Vec<FailedSetup>,
    /// The removed workspaces named, by ID, which open again.
    pub removed: Vec<SpaceSelector>,
}

impl Selected {
    /// The spaces `targets` names, each once; every space with stopped work
    /// for `--all`; otherwise the current workspace when it has something
    /// to restore, else those picked among the workspaces with stopped
    /// work, the stopped chats and the removed workspaces. `None` once it
    /// listed those, without a terminal or anything to pick.
    pub async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Option<Self>> {
        if targets.all {
            return Ok(Some(Self::default()));
        }
        let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
        let stopped = stopped_chats(&spaces);
        let removed = removed_workspaces(ctx, &spaces, &targets.space).await?;
        let entries = match targets.space.is_empty() {
            true => removed_entries(&removed),
            false => ui::Entries::new(),
        };
        match ui::select_restorable(ctx, targets.space, &stopped, entries).await? {
            Some(selectors) => Ok(Some(Self::resolved(&spaces, &removed, selectors))),
            None => {
                list(ctx, &spaces, stopped, removed).await?;
                Ok(None)
            }
        }
    }

    /// The spaces `selectors` name among `spaces` and the `removed`
    /// workspaces, each once.
    fn resolved(
        spaces: &Spaces,
        removed: &[RemovedWorkspace],
        selectors: Vec<SpaceSelector>,
    ) -> Self {
        let mut selected = Self {
            spaces: Some(Vec::new()),
            ..Self::default()
        };
        for selector in selectors {
            selected.add(resolve(spaces, removed, selector));
        }
        selected
    }

    /// Refuse the running chats selected, which have nothing to restore.
    pub fn refuse_running(&self) -> Result<()> {
        if let Some(chat) = self.running.first() {
            bail!("chat {} is running; nothing to restore", chat.name);
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
            Target::Removed(workspace) if !self.removed.contains(&workspace) => {
                self.removed.push(workspace);
            }
            Target::Removed(_) => {}
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

/// The removed workspaces, which `names` may name, or which a picker or
/// list offers when it names none; none when every name is a space's.
async fn removed_workspaces(
    ctx: &Context,
    spaces: &Spaces,
    names: &[SpaceSelector],
) -> Result<Vec<RemovedWorkspace>> {
    if !names.is_empty() && names.iter().all(|name| spaces.find(name).is_some()) {
        return Ok(Vec::new());
    }
    crate::client::removal::removed(&ctx.paths).await
}

/// The picker entries of the `removed` workspaces, by ID.
fn removed_entries(removed: &[RemovedWorkspace]) -> ui::Entries {
    removed
        .iter()
        .map(|workspace| workspace.workspace_id.to_string())
        .zip(super::reopen::removed_lines(removed))
        .collect()
}

/// List what a restore without a space picks among: the workspaces of
/// `spaces` with stopped work, the `stopped` chats and the `removed`
/// workspaces.
async fn list(
    ctx: &Context,
    spaces: &Spaces,
    chats: Vec<Chat>,
    removed: Vec<RemovedWorkspace>,
) -> Result<()> {
    let summaries = ui::saved_summaries(&ctx.paths).await;
    let mut stopped: Vec<_> = (spaces.workspaces.iter())
        .filter_map(|workspace| {
            let saved = summaries.get(&workspace.id)?.clone();
            let workspace = workspace.clone();
            Some(StoppedWorkspace { workspace, saved })
        })
        .collect();
    stopped.sort_by_key(|stopped| !stopped.saved.unfinished);
    let restorable = Restorable {
        stopped,
        chats,
        removed,
    };
    ctx.show(&restorable, print_restorable)
}

/// Print `restorable` by group, each workspace and chat led by its number
/// and each removed workspace by its ID, which select them.
fn print_restorable(restorable: &Restorable) {
    let Restorable {
        stopped,
        chats,
        removed,
    } = restorable;
    if stopped.is_empty() && chats.is_empty() && removed.is_empty() {
        outln!("Nothing to restore");
    }
    if !stopped.is_empty() {
        outln!("Stopped work:");
    }
    for StoppedWorkspace { workspace, saved } in stopped {
        let (what, _) = ui::restorable(Some(saved)).unwrap_or_default();
        outln!("  {}  {}  {what}", workspace.number, workspace.name);
    }
    if !chats.is_empty() {
        outln!("Stopped chats:");
    }
    for chat in chats {
        outln!("  {}  {}", chat.number, chat.name);
    }
    if !removed.is_empty() {
        outln!("Removed workspaces:");
    }
    for (workspace, line) in removed.iter().zip(super::reopen::removed_lines(removed)) {
        outln!("  {}  {line}", workspace.workspace_id);
    }
}

#[cfg(test)]
mod tests;
