//! The threads a space opened that other machines keep, its comments of
//! the reviews they keep of their workspaces: how many wait, which keeps
//! the space from cleanup as its waiting threads here do, and the machines
//! that close and reopen them as its own machine closes and reopens its
//! own.
use std::{collections::HashMap, sync::Mutex, time::Duration};

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::hub::{Member, directory::Directory};
use tokio::time::Instant;

use crate::{Hub, daemon::Daemon, log};

/// How long a reading of the threads that wait elsewhere stands, so a
/// cleanup pass or a status, which ask for every space or twice for one,
/// read the hub's directory once; the reports it lists lag behind their
/// machines about as long.
const FRESH: Duration = Duration::from_secs(5);

/// How many threads each space opened on other machines wait for their
/// holder, as last read, and when.
#[derive(Default)]
pub(super) struct Waiting(Mutex<Option<(Instant, HashMap<SpaceId, usize>)>>);

impl Waiting {
    /// How many threads the space `id` opened elsewhere wait, while the
    /// last reading is fresh.
    fn of(&self, id: &SpaceId) -> Option<usize> {
        let last = self.0.lock().unwrap();
        let (_, counts) = last.as_ref().filter(|(read, _)| read.elapsed() < FRESH)?;
        Some(counts.get(id).copied().unwrap_or_default())
    }
}

impl Hub {
    /// How many threads the space `id` opened on other machines wait for
    /// their holder, as the hub lists them, or as it last did while it
    /// cannot be asked, read at most once every [`FRESH`]; none while
    /// neither can be read.
    pub async fn waiting_elsewhere(&self, daemon: &dyn Daemon, id: &SpaceId) -> usize {
        if let Some(count) = self.waiting.of(id) {
            return count;
        }
        let counts = self.read_waiting(daemon).await;
        let count = counts.get(id).copied().unwrap_or_default();
        *self.waiting.0.lock().unwrap() = Some((Instant::now(), counts));
        count
    }

    /// How many threads each space opened on other machines wait for their
    /// holder, as the hub lists them, or as it last did.
    async fn read_waiting(&self, daemon: &dyn Daemon) -> HashMap<SpaceId, usize> {
        match self.directory_or_known(daemon).await {
            Ok(Some((machine, directory))) => waiting_by_opener(&machine, &directory),
            Ok(None) => HashMap::new(),
            Err(error) => {
                log!("hub: could not read the threads spaces opened elsewhere: {error:#}");
                HashMap::new()
            }
        }
    }

    /// The joined machines other than this one, as the hub lists them, or
    /// as it last did while it cannot be asked; none in no hub.
    pub async fn other_machines(&self, daemon: &dyn Daemon) -> Vec<Member> {
        match self.directory_or_known(daemon).await {
            Ok(Some((machine, directory))) => (directory.machines.into_iter())
                .filter(|member| member.id != machine)
                .collect(),
            Ok(None) => Vec::new(),
            Err(error) => {
                log!("hub: could not read the other machines: {error:#}");
                Vec::new()
            }
        }
    }
}

/// How many threads each space opened on the machines of `directory` other
/// than `machine` wait for their holder.
fn waiting_by_opener(
    machine: &shoal_ids::member_ids::MemberId,
    directory: &Directory,
) -> HashMap<SpaceId, usize> {
    let mut counts = HashMap::new();
    let threads = (directory.spaces.iter())
        .filter(|listed| &listed.machine != machine)
        .flat_map(|listed| &listed.space.threads);
    for thread in threads.filter(|thread| thread.is_waiting()) {
        if let Some(opener) = thread.opener_id() {
            *counts.entry(opener.clone()).or_default() += 1;
        }
    }
    counts
}
