//! The processes a chat owns, which ending it stops: those carrying its
//! identity, those its process started, and the members of the process group
//! it leads, except a workspace's, such as an execution a command in the
//! chat started, which goes on without it.
use crate::process::identity::IdentityExt;
use anyhow::{Result, bail};

use crate::process::identity::{self, Identity};

/// How many times ending a chat stops the processes it owns, since a process
/// may start another while it stops.
const ROUNDS: usize = 5;

/// The processes the chat `chat`, saved while it ran as `recorded`, may
/// still own after a stop that failed or was interrupted, having signalled
/// `stopped`, other than the execution `wrappers`, which hold its restore or
/// discard: those it [`owned`], and the members of the group it led, which
/// keeps its ID while any member remains, other than a workspace's. A group
/// under that ID is another's once a different process has the ID, or the
/// process ran in an earlier boot. Membership alone does not prove
/// ownership, so only refusing counts it.
pub(super) async fn survivors(
    chat: &str,
    recorded: &Identity,
    stopped: &[Identity],
    wrappers: &[Identity],
) -> Result<Vec<Identity>> {
    let mut survivors = owned(chat, recorded, stopped, wrappers).await?;
    let (_, members) = match led_group(recorded)? {
        Some(group) => identity::related(None, Some(group), None).await?,
        None => Default::default(),
    };
    for member in members {
        if identity::group_of(&member) == Some(recorded.pid)
            && !identity::owned_by_workspace(&member, wrappers)?
            && !survivors.contains(&member)
        {
            survivors.push(member);
        }
    }
    Ok(survivors)
}

/// The processes the chat `chat`, saved while it ran as `recorded`, is
/// proven to own, other than the execution `wrappers`, which a stop may
/// signal: that process and those of `stopped`, which it owned, while they
/// live, those carrying the chat's identity, and those they started, by the
/// lineage of their never-reused IDs where recorded.
pub(super) async fn owned(
    chat: &str,
    recorded: &Identity,
    stopped: &[Identity],
    wrappers: &[Identity],
) -> Result<Vec<Identity>> {
    let mut owned: Vec<Identity> = identity::scan_chat(chat)
        .await?
        .into_iter()
        .filter(|process| !wrappers.contains(process))
        .collect();
    let ancestors = [std::slice::from_ref(recorded), stopped].concat();
    let started = identity::started_by(&ancestors, wrappers).await?;
    for process in started.into_iter().chain(living(&ancestors)?) {
        if !owned.contains(&process) {
            owned.push(process);
        }
    }
    Ok(owned)
}

/// Those of `processes` still running.
fn living(processes: &[Identity]) -> Result<Vec<Identity>> {
    let mut living = Vec::new();
    for process in processes {
        if identity::alive(process)? {
            living.push(process.clone());
        }
    }
    Ok(living)
}

/// Stop what the stopped chat `chat`, whose process `recorded` ended, is
/// proven to own, other than the execution `wrappers`, again while stopping
/// it starts others, which descend from what each round and the earlier
/// stops that signalled `stopped` stopped. Each round first has `record`
/// keep what it signals.
pub(super) async fn stop_owned<R>(
    chat: &str,
    recorded: &Identity,
    stopped: Vec<Identity>,
    wrappers: &[Identity],
    record: impl Fn(Vec<Identity>) -> R,
) -> Result<()>
where
    R: Future<Output = Result<()>>,
{
    stop_in_rounds(
        stopped,
        |stopped| async move { owned(chat, recorded, &stopped, wrappers).await },
        record,
    )
    .await
}

/// Stop the processes `owned`, given those `stopped` before, finds, again
/// while stopping them starts others, refusing with those still running
/// after the last round. `record` keeps what each round signals before it
/// does, so a stop interrupted after signalling them leaves what they start
/// traceable.
async fn stop_in_rounds<F, Fut, R>(
    mut stopped: Vec<Identity>,
    owned: F,
    record: impl Fn(Vec<Identity>) -> R,
) -> Result<()>
where
    F: Fn(Vec<Identity>) -> Fut,
    Fut: Future<Output = Result<Vec<Identity>>>,
    R: Future<Output = Result<()>>,
{
    for _ in 0..ROUNDS {
        let found = owned(stopped.clone()).await?;
        if found.is_empty() {
            return Ok(());
        }
        record(found.clone()).await?;
        identity::stop_verified(&found).await?;
        for process in found {
            if !stopped.contains(&process) {
                stopped.push(process);
            }
        }
    }
    let pids: Vec<String> = owned(stopped)
        .await?
        .iter()
        .map(|process| process.pid.to_string())
        .collect();
    bail!(
        "the chat's processes keep starting others; end them with kill {}",
        pids.join(" ")
    )
}

/// The group the process `recorded` led, when it has to be asked: its
/// recorded never-reused ID, where the platform reports one, finds what it
/// started through their lineage, which a group under a reused ID cannot
/// fool. Otherwise when one under its ID can still be its own: it runs, or
/// no other process has its ID, in the boot it ran in.
pub(super) fn led_group(recorded: &Identity) -> Result<Option<u32>> {
    if recorded.unique.is_some() || recorded.of_earlier_boot() {
        return Ok(None);
    }
    let reused = identity::capture(recorded.pid)?.is_some_and(|current| current != *recorded);
    Ok((!reused).then_some(recorded.pid))
}

/// The processes of a running chat.
pub(super) struct ChatProcesses<'a> {
    chat: &'a str,
    /// The wrappers of the executions running, which belong to their
    /// workspaces wherever they started.
    wrappers: Vec<Identity>,
    /// The process the chat lasts while, with the unique ID its lineage is
    /// followed by.
    process: Identity,
    /// The process group it leads, which its descendants keep, unless they
    /// leave it, even once they replace their image.
    group: Option<u32>,
}

impl<'a> ChatProcesses<'a> {
    /// The processes of chat `chat`, whose process was `recorded`, beside
    /// the execution `wrappers`; `None` once that process has ended.
    pub(super) fn of(
        chat: &'a str,
        recorded: &Identity,
        wrappers: Vec<Identity>,
    ) -> Result<Option<Self>> {
        let Some(process) =
            identity::capture(recorded.pid)?.filter(|process| process.birth == recorded.birth)
        else {
            return Ok(None);
        };
        let group = identity::group_of(&process).filter(|group| *group == process.pid);
        Ok(Some(Self {
            chat,
            wrappers,
            process,
            group,
        }))
    }

    /// Stop every process the chat owns, again while stopping them starts
    /// others, which descend from what each round and the earlier stops that
    /// signalled `stopped` stopped, refusing with those still running after
    /// the last round. Each round first has `record` keep what it signals.
    pub(super) async fn stop<R>(
        &self,
        stopped: Vec<Identity>,
        record: impl Fn(Vec<Identity>) -> R,
    ) -> Result<()>
    where
        R: Future<Output = Result<()>>,
    {
        stop_in_rounds(
            stopped,
            |stopped| async move { self.owned(&stopped).await },
            record,
        )
        .await
    }

    /// The live processes the chat owns now, given those stops of it
    /// already signalled. Each is verified as its own: carrying the chat's
    /// identity, its process or one of `stopped` still running, descended by
    /// unique IDs from one of them, or still in its group once captured,
    /// which a process that took a reused PID cannot join; none is a
    /// workspace's.
    async fn owned(&self, stopped: &[Identity]) -> Result<Vec<Identity>> {
        // A wrapper started in the chat carries its marker, not its workspace's.
        let mut owned: Vec<Identity> = identity::scan_chat(self.chat)
            .await?
            .into_iter()
            .filter(|process| !self.wrappers.contains(process))
            .collect();
        let ancestors = [std::slice::from_ref(&self.process), stopped].concat();
        let started = identity::started_by(&ancestors, &self.wrappers).await?;
        let grouped = self.grouped().await?;
        let living = living(&ancestors)?;
        for process in started.into_iter().chain(grouped).chain(living) {
            if !owned.contains(&process) {
                owned.push(process);
            }
        }
        Ok(owned)
    }

    /// The live members of the chat's process group, when its process leads
    /// one, other than a workspace's.
    async fn grouped(&self) -> Result<Vec<Identity>> {
        let (_, members) = identity::related(None, self.group, None).await?;
        let mut grouped = Vec::new();
        for member in members {
            if identity::group_of(&member) == self.group
                && !identity::owned_by_workspace(&member, &self.wrappers)?
            {
                grouped.push(member);
            }
        }
        Ok(grouped)
    }
}

#[cfg(test)]
mod tests;
