//! What `shoal resume` acts on: workspaces with stopped work and the chats
//! Shoal saved, which share one namespace of names.
use anyhow::{Result, bail};

use crate::{
    cli::{args::SpaceTargets, context::Context, ui},
    model::{Chat, PresentSpace, Spaces},
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
    /// A workspace by ID, or a selector no space has, which the daemon
    /// refuses.
    Workspace(String),
}

/// The space `selector` names among `spaces`: a stopped chat or a workspace.
/// A running chat has nothing to resume.
pub(super) fn resolve(spaces: &Spaces, selector: String) -> Result<Target> {
    match spaces.find(&selector) {
        Some(PresentSpace::Workspace(workspace)) => Ok(Target::Workspace(workspace.id)),
        Some(PresentSpace::Chat(chat)) if chat.restore.is_some() => Ok(Target::Chat(chat)),
        Some(PresentSpace::Chat(chat)) => bail!("chat {} is running; nothing to resume", chat.name),
        None => Ok(Target::Workspace(selector)),
    }
}

/// The spaces a resume acts on.
#[derive(Debug, Default)]
pub(super) struct Selected {
    pub chats: Vec<Chat>,
    /// By ID, or by a selector the daemon refuses; every workspace with
    /// stopped work for `None`.
    pub workspaces: Option<Vec<String>>,
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
            Some(selectors) => Self::resolved(&spaces, selectors),
            None => Ok(Self::every(saved)),
        }
    }

    /// The spaces `selectors` name among `spaces`, each once.
    fn resolved(spaces: &Spaces, selectors: Vec<String>) -> Result<Self> {
        let mut selected = Self {
            chats: Vec::new(),
            workspaces: Some(Vec::new()),
        };
        for selector in selectors {
            selected.add(resolve(spaces, selector)?);
        }
        Ok(selected)
    }

    /// The `saved` chats and every workspace.
    fn every(saved: Vec<Chat>) -> Self {
        Self {
            chats: saved,
            workspaces: None,
        }
    }

    /// Add `target` unless it is already selected.
    fn add(&mut self, target: Target) {
        match target {
            Target::Chat(chat) if !self.chats.contains(&chat) => self.chats.push(chat),
            Target::Chat(_) => {}
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

#[cfg(test)]
mod tests;
