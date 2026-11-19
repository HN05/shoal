//! What `shoal restore` acts on: workspaces with stopped work, the chats
//! Shoal saved, the running spaces whose agents an interrupt left idle and
//! removed spaces, which one lookup finds by number or ID; without a
//! space, the current one or those picked, or else a list of them all.
use anyhow::{Result, bail};

use crate::cli::commands::resume::setups::FailedSetup;
use crate::client::output::restore::{Restorable, StoppedWorkspace};
use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    model::{Chat, RemovedSpace, Space, Spaces},
};
use shoal_protocol::space_names::SpaceSelector;

/// The space a `shoal restore` target names.
#[derive(Debug, PartialEq)]
pub(in crate::cli::commands) enum Target {
    /// A running chat, whose agent may continue the turn an interrupt
    /// ended.
    Running(Box<Chat>),
    /// A space by ID, a workspace or a stopped chat, or a selector no space
    /// has, which the daemon refuses.
    Space(SpaceSelector),
    /// A removed space, which a restore opens again.
    Removed(Box<RemovedSpace>),
}

/// The space `selector` names among `spaces` and the `removed` ones: a
/// running chat, another space or a removed space.
pub(super) fn resolve(
    spaces: &Spaces,
    removed: &[RemovedSpace],
    selector: SpaceSelector,
) -> Target {
    match spaces.find(&selector).map(crate::model::Space::into_chat) {
        Some(Ok(chat)) if chat.running => Target::Running(Box::new(chat)),
        Some(Ok(Space { id, .. }) | Err(Space { id, .. })) => Target::Space(SpaceSelector::id(&id)),
        None => match (removed.iter()).find(|space| selector.given_id() == Some(&space.space_id)) {
            Some(space) => Target::Removed(Box::new(space.clone())),
            None => Target::Space(selector),
        },
    }
}

/// The spaces a restore acts on.
#[derive(Debug, Default)]
pub(in crate::cli::commands) struct Selected {
    /// The running chats named, which have nothing to restore but a turn an
    /// interrupt ended.
    pub running: Vec<Chat>,
    /// By ID, or by a selector the daemon refuses; every space with stopped
    /// work for `None`.
    pub spaces: Option<Vec<SpaceSelector>>,
    /// The workspaces named whose setup failed, which restore by retrying
    /// it, once [`crate::cli::commands::resume::setups::take_failed`] takes them from the
    /// others.
    pub setups: Vec<FailedSetup>,
    /// The failed launches of the spaces named, which restore by queuing
    /// them again, once [`crate::cli::commands::resume::failed_starts::take`] takes them
    /// from the others.
    pub failed_starts: Vec<crate::model::QueueEntry>,
    /// The removed spaces named, which open again in that order.
    pub removed: Vec<RemovedSpace>,
}

impl Selected {
    /// The spaces `targets` names, each once; every space with stopped work
    /// for `--all`; otherwise the current workspace when it has something
    /// to restore, else those picked among the workspaces with stopped
    /// work, the stopped chats and the removed spaces. `None` once it
    /// listed those, without a terminal or anything to pick.
    pub async fn load(ctx: &Context, targets: SpaceTargets) -> Result<Option<Self>> {
        if targets.all {
            return Ok(Some(Self::default()));
        }
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        let stopped = stopped_chats(&spaces);
        let removed = removed_spaces(ctx, &spaces, &targets.space).await?;
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

    /// The spaces `selectors` name among `spaces` and the `removed` ones,
    /// each once.
    fn resolved(spaces: &Spaces, removed: &[RemovedSpace], selectors: Vec<SpaceSelector>) -> Self {
        let mut selected = Self {
            spaces: Some(Vec::new()),
            ..Self::default()
        };
        for selector in selectors {
            selected.add(resolve(spaces, removed, selector));
        }
        // A workspace opens before a chat in its worktree.
        (selected.removed).sort_by_key(|removed| removed.workspace().is_none());
        selected
    }

    /// Whether nothing is selected but running chats, as once the spaces
    /// whose interrupted turns continue are taken from the selection.
    pub fn is_empty(&self) -> bool {
        let Self {
            running: _,
            spaces,
            setups,
            failed_starts,
            removed,
        } = self;
        spaces.as_ref().is_some_and(Vec::is_empty)
            && setups.is_empty()
            && failed_starts.is_empty()
            && removed.is_empty()
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
            Target::Removed(space) => {
                let id = &space.space_id;
                if !self.removed.iter().any(|named| named.space_id == *id) {
                    self.removed.push(*space);
                }
            }
        }
    }
}

/// The stopped chats among `spaces`, which a picker offers beside the
/// workspaces.
fn stopped_chats(spaces: &Spaces) -> Vec<Chat> {
    (spaces.chats().into_iter())
        .filter(|chat| !chat.running)
        .collect()
}

/// The removed spaces the caller may restore, as the daemon decides,
/// workspaces before chats, which `names` may name, or which a picker or
/// list offers when it names none; none when every name is a space's.
async fn removed_spaces(
    ctx: &Context,
    spaces: &Spaces,
    names: &[SpaceSelector],
) -> Result<Vec<RemovedSpace>> {
    if !names.is_empty() && names.iter().all(|name| spaces.find(name).is_some()) {
        return Ok(Vec::new());
    }
    let mut removed = crate::client::removal::removed(&ctx.paths).await?;
    // A removed space the list leaves out, the daemon refuses with why.
    for name in names.iter().filter(|name| spaces.find(name).is_none()) {
        if name.given_id().is_some()
            && !(removed.iter()).any(|removed| name.given_id() == Some(&removed.space_id))
        {
            removed.push(crate::client::removal::removed_space(&ctx.paths, name.clone()).await?);
        }
    }
    removed.sort_by_key(|removed| removed.workspace().is_none());
    Ok(removed)
}

/// The picker entries of the `removed` spaces, by ID.
fn removed_entries(removed: &[RemovedSpace]) -> ui::Entries {
    removed
        .iter()
        .map(|space| space.space_id.to_string())
        .zip(super::reopen::removed_lines(removed))
        .collect()
}

/// List what a restore without a space picks among: the workspaces of
/// `spaces` with stopped work, the `stopped` chats and the `removed`
/// spaces.
async fn list(
    ctx: &Context,
    spaces: &Spaces,
    chats: Vec<Chat>,
    removed: Vec<RemovedSpace>,
) -> Result<()> {
    let summaries = ui::saved_summaries(&ctx.paths).await;
    let mut stopped: Vec<_> = (spaces.workspaces().into_iter())
        .filter_map(|workspace| {
            let saved = summaries.get(&workspace.id)?.clone();
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
/// and each removed space by its ID, which select them.
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
    let lines = super::reopen::removed_lines(removed);
    let (workspaces, chats): (Vec<_>, Vec<_>) =
        (removed.iter().zip(lines)).partition(|(space, _)| space.workspace().is_some());
    for (heading, group) in [
        ("Removed workspaces:", workspaces),
        ("Removed chats:", chats),
    ] {
        if !group.is_empty() {
            outln!("{heading}");
        }
        for (space, line) in group {
            outln!("  {}  {line}", space.space_id);
        }
    }
}

#[cfg(test)]
mod tests;
