//! The hub's open links, at most one per member, with the calls the hub
//! sends each. A second link that presents a credential an open one holds
//! is refused, unless it comes from the same installation, whose earlier
//! link it then replaces: that one was lost, as a laptop's is when it
//! sleeps.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::availability::Classified;
use tokio::sync::Notify;

use super::session::Calls;

#[derive(Default)]
pub(super) struct Links {
    open: Mutex<HashMap<MemberId, Open>>,
    next: AtomicU64,
}

struct Open {
    instance: String,
    /// Which link of the member this is.
    token: u64,
    stop: Arc<Notify>,
    /// The hub's calls to the member, once its session serves the link.
    calls: Option<Arc<Calls>>,
    /// The availability the hub last told the member it has, once it
    /// described itself.
    told: Option<Classified>,
}

/// A member's open link, which leaves [`Links`] when dropped.
pub(super) struct Registration<'a> {
    links: &'a Links,
    member: MemberId,
    token: u64,
    stop: Arc<Notify>,
}

impl Registration<'_> {
    /// Resolves once another link of the same installation replaces this
    /// one, or the hub revokes the member.
    pub(super) async fn stopped(&self) {
        self.stop.notified().await
    }

    /// Send the hub's calls to the member through `calls` while this link
    /// stays open.
    pub(super) fn serve_calls(&self, calls: Arc<Calls>) {
        let mut open = self.links.open.lock().unwrap();
        if let Some(open) = (open.get_mut(&self.member)).filter(|open| open.token == self.token) {
            open.calls = Some(calls);
        }
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let mut open = self.links.open.lock().unwrap();
        if open
            .get(&self.member)
            .is_some_and(|open| open.token == self.token)
        {
            open.remove(&self.member);
        }
    }
}

impl Links {
    /// Register `member`'s link from `instance`, none while another
    /// installation's link holds its credential.
    pub(super) fn register(&self, member: &MemberId, instance: &str) -> Option<Registration<'_>> {
        let mut open = self.open.lock().unwrap();
        if let Some(earlier) = open.get(member) {
            if earlier.instance != instance {
                return None;
            }
            earlier.stop.notify_one();
        }
        let token = self.next.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(Notify::new());
        let registered = Open {
            instance: instance.to_owned(),
            token,
            stop: stop.clone(),
            calls: None,
            told: None,
        };
        open.insert(member.clone(), registered);
        Some(Registration {
            links: self,
            member: member.clone(),
            token,
            stop,
        })
    }

    /// End `member`'s link, if one is open.
    pub(super) fn stop(&self, member: &MemberId) {
        if let Some(open) = self.open.lock().unwrap().get(member) {
            open.stop.notify_one();
        }
    }

    /// End every open link, as once the daemon no longer takes the hub
    /// role.
    pub(super) fn stop_all(&self) {
        for open in self.open.lock().unwrap().values() {
            open.stop.notify_one();
        }
    }

    /// The hub's calls to `member`, while its link is open.
    pub(super) fn calls(&self, member: &MemberId) -> Option<Arc<Calls>> {
        let open = self.open.lock().unwrap();
        open.get(member).and_then(|open| open.calls.clone())
    }

    /// The members whose links are open.
    pub(super) fn connected(&self) -> HashSet<MemberId> {
        self.open.lock().unwrap().keys().cloned().collect()
    }

    /// Remember that `member`'s open link was told `availability`.
    pub(super) fn told(&self, member: &MemberId, availability: Classified) {
        if let Some(open) = self.open.lock().unwrap().get_mut(member) {
            open.told = Some(availability);
        }
    }

    /// The machines whose links are open and that described themselves,
    /// with the hub's calls to each and the availability it was told.
    pub(super) fn described(&self) -> HashMap<MemberId, (Arc<Calls>, Classified)> {
        let open = self.open.lock().unwrap();
        (open.iter())
            .filter_map(|(member, open)| Some((member.clone(), (open.calls.clone()?, open.told?))))
            .collect()
    }
}

#[cfg(test)]
mod tests;
