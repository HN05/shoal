//! What `shoal resume` acts on: the spaces named, workspaces and chats
//! alike, wherever they run, every space with a stopped agent for `--all`,
//! or the spaces of the machines `--machine` names that `--all` would take
//! there; without a space, the current workspace when its agent can
//! resume, else those picked among the spaces whose agent can resume, on
//! this machine and the other joined machines, or else a list of them.
use anyhow::Result;
use std::collections::HashSet;

use crate::client::output::{
    agents::Skipped,
    resume::{Resumable, ResumableSpace},
};
use crate::{
    cli::{
        args::SpaceTargets,
        commands::{
            queued_agents,
            space_selection::{self, on_machine},
        },
        context::Context,
        everywhere, ui,
    },
    model::{Space, Spaces},
    state::QueueState,
    time::UnixTime,
};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::hub::{
    Member,
    directory::{HubView, Listed, Resumable as Reported},
};
use shoal_protocol::model::Located;
use shoal_protocol::space_names::{SpaceName, SpaceSelector};

/// The spaces a resume acts on.
#[derive(Debug)]
pub(in crate::cli::commands) enum Selected {
    /// Every space in reach with something to resume, for `--all`.
    All,
    /// The spaces named or picked.
    Named(Vec<Target>),
    /// The spaces `--machine` selects, and those of an offline machine it
    /// skips, since a resume never waits for its machine.
    OnMachines {
        spaces: Vec<Target>,
        skipped: Vec<Skipped>,
    },
}

/// A space a resume acts on: by its ID where a machine has it, else as
/// given for its daemon to refuse.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::cli::commands) struct Target {
    pub selector: SpaceSelector,
    /// Its number and name, where a machine has it.
    pub space: Option<(SpaceNumber, SpaceName)>,
    /// The other joined machine it runs on; none for this machine's.
    pub machine: Option<Member>,
}

/// What a command takes of the spaces of the machines `--machine` names,
/// as its `--all` takes them here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::commands) enum Taking {
    /// `shoal resume`: the spaces whose saved work `--all` resumes.
    Resumes,
    /// `restore --discard-agent`: every space with something to resume.
    Discards,
}

impl Selected {
    /// The spaces `targets` selects for what `taking` does with them: every
    /// space in reach with something to resume for `--all`, those on the
    /// machines `--machine` names that `--all` would take there, else
    /// those named, else the current workspace when its agent can resume,
    /// else those picked among the spaces whose agent can resume. `None`
    /// once it listed those, without a terminal or anything to pick.
    pub async fn load(
        ctx: &Context,
        targets: SpaceTargets,
        taking: Taking,
    ) -> Result<Option<Self>> {
        if targets.all {
            return Ok(Some(Self::All));
        }
        if !targets.machines.is_empty() {
            return on_machines(ctx, targets.machines, taking).await.map(Some);
        }
        let spaces = crate::client::spaces::list(&ctx.paths).await?;
        let selectors = match targets.space.is_empty() {
            false => ui::named_spaces(ctx, targets.space).await?,
            true => match picked(ctx, &spaces).await? {
                Some(picked) => picked,
                None => return Ok(None),
            },
        };
        let selectors = resolved(&spaces, selectors);
        Ok(Some(Self::Named(located(ctx, &spaces, selectors).await)))
    }
}

/// The spaces picked among those whose agent can resume, on this machine
/// and the other joined machines, or the current workspace when its agent
/// can; `None` once they are listed instead, without a terminal or
/// anything to pick.
async fn picked(ctx: &Context, spaces: &Spaces) -> Result<Option<Vec<SpaceSelector>>> {
    let resumable = resumable(ctx, spaces).await?;
    let elsewhere = everywhere::elsewhere(ctx, "the other machines' spaces").await;
    let picked = ui::select_resumable(ctx, &resumable, elsewhere).await?;
    if picked.is_none() {
        let resumable = Resumable { spaces: resumable };
        ctx.show(&resumable, print_resumable)?;
    }
    Ok(picked)
}

/// The spaces `selectors` name among `spaces`, by ID and each once; a
/// selector no space here has stays as given, for the machine that has it,
/// or else the daemon, to refuse.
fn resolved(spaces: &Spaces, selectors: Vec<SpaceSelector>) -> Vec<SpaceSelector> {
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
    resolved
}

/// The targets `selectors` name: this machine's among `spaces`, else
/// another machine's as the hub lists it, else as given.
async fn located(ctx: &Context, spaces: &Spaces, selectors: Vec<SpaceSelector>) -> Vec<Target> {
    let elsewhere = match selectors
        .iter()
        .all(|selector| spaces.find(selector).is_some())
    {
        true => None,
        false => everywhere::elsewhere(ctx, "the other machines' spaces").await,
    };
    (selectors.into_iter())
        .map(|selector| match spaces.find(&selector) {
            Some(space) => Target::here(&space),
            None => Target::named(selector, elsewhere.as_ref()),
        })
        .collect()
}

/// The spaces of the joined machines `machines` names that `--all` would
/// take there for what `taking` does: another machine's as it last
/// reported them, skipped while it is offline, and this machine's as
/// `--all` takes them here.
async fn on_machines(ctx: &Context, machines: Vec<String>, taking: Taking) -> Result<Selected> {
    let located = space_selection::on_machines(ctx, machines).await?;
    let here: HashSet<SpaceId> = match located
        .iter()
        .any(|located| matches!(located, Located::Here(_)))
    {
        true => (taking.in_reach(ctx).await?.into_iter())
            .filter_map(|target| target.selector.given_id().cloned())
            .collect(),
        false => HashSet::new(),
    };
    let now = UnixTime::now();
    let (mut spaces, mut skipped) = (Vec::new(), Vec::new());
    for located in located {
        match located {
            Located::Here(space) if here.contains(&space.id) => spaces.push(Target::here(&space)),
            Located::Elsewhere { space, machine } if taking.takes(&space) => {
                match machine.connected {
                    true => spaces.push(Target::elsewhere(&space, *machine)),
                    false => skipped.push(Skipped {
                        space: space.space.name.clone(),
                        agent: None,
                        reason: machine.offline(now),
                        machine: Some(machine.name),
                    }),
                }
            }
            _ => {}
        }
    }
    Ok(Selected::OnMachines { spaces, skipped })
}

impl Taking {
    /// Whether it takes another machine's space `listed`, as that machine
    /// last reported it.
    fn takes(self, listed: &Listed) -> bool {
        match (self, listed.space.resumable) {
            (Self::Resumes, resumable) => resumable == Some(Reported::Saved),
            (Self::Discards, resumable) => resumable.is_some(),
        }
    }

    /// The spaces of this machine in reach that its `--all` takes.
    pub(in crate::cli::commands) async fn in_reach(self, ctx: &Context) -> Result<Vec<Target>> {
        match self {
            Self::Resumes => resumable_in_reach(ctx).await,
            Self::Discards => discardable_in_reach(ctx).await,
        }
    }
}

impl Target {
    /// This machine's `space`.
    pub(in crate::cli::commands) fn here(space: &Space) -> Self {
        Self {
            selector: SpaceSelector::id(&space.id),
            space: Some((space.number, space.name.clone())),
            machine: None,
        }
    }

    /// The space of another machine that `listed` lists on `machine`.
    fn elsewhere(listed: &Listed, machine: Member) -> Self {
        let space = &listed.space;
        Self {
            selector: SpaceSelector::id(&space.id),
            space: Some((space.number, space.name.clone())),
            machine: Some(machine),
        }
    }

    /// The space `selector` names that this machine has none of: another
    /// machine's as `elsewhere` lists it, else as given.
    fn named(selector: SpaceSelector, elsewhere: Option<&HubView>) -> Self {
        let listed = elsewhere.and_then(|view| Some((view, view.selected(&selector).ok()??)));
        match listed {
            Some((view, listed)) => {
                let machine = view.directory.machine(&listed.machine).cloned();
                let machine = machine.unwrap_or_else(|| Member::unlisted(listed.machine.clone()));
                Self::elsewhere(listed, machine)
            }
            None => Self {
                selector,
                space: None,
                machine: None,
            },
        }
    }

    /// The name of the other machine it runs on.
    pub(in crate::cli::commands) fn machine_name(&self) -> Option<String> {
        self.machine.as_ref().map(|machine| machine.name.clone())
    }

    /// Its name, or else as given, with the other machine it runs on, as
    /// reports show it.
    pub(in crate::cli::commands) fn shown(&self) -> String {
        let name = match &self.space {
            Some((_, name)) => name.to_string(),
            None => self.selector.to_string(),
        };
        on_machine(&name, self.machine_name().as_deref())
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
pub(super) async fn resumable_in_reach(ctx: &Context) -> Result<Vec<Target>> {
    Ok((in_reach(ctx).await?.into_iter())
        .filter(|resumable| resumable.saved.is_some() && !resumable.space.done())
        .map(|resumable| Target::here(&resumable.space))
        .collect())
}

/// The spaces in reach whose agents `--all` discards, by ID: every one with
/// saved work or a failed launch, done or not.
pub(in crate::cli::commands) async fn discardable_in_reach(ctx: &Context) -> Result<Vec<Target>> {
    Ok((in_reach(ctx).await?.into_iter())
        .map(|resumable| Target::here(&resumable.space))
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
