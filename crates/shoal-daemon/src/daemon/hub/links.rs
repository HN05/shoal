//! The hub's open links, at most one per member. A second link that presents
//! a credential an open one holds is refused, unless it comes from the same
//! installation, whose earlier link it then replaces: that one was lost, as
//! a laptop's is when it sleeps.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use shoal_ids::member_ids::MemberId;
use tokio::sync::Notify;

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

    /// The members whose links are open.
    pub(super) fn connected(&self) -> HashSet<MemberId> {
        self.open.lock().unwrap().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests;
