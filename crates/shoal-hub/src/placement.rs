//! The capacity each machine of the hub reports with its spaces, which the
//! hub keeps in memory, for as long as it runs, and lists with its members.
use std::{collections::HashMap, sync::Mutex};

use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::Member;
use shoal_schedule::placement::snapshot::Capacity;

use crate::Hub;

/// The capacity each machine last reported.
#[derive(Default)]
pub(crate) struct Capacities(Mutex<HashMap<MemberId, Capacity>>);

impl Capacities {
    /// Keep `capacity` as what the machine `member` reported last.
    pub fn keep(&self, member: &MemberId, capacity: Capacity) {
        let mut capacities = self.0.lock().unwrap();
        capacities.insert(member.clone(), capacity);
    }

    /// What the machine `member` reported last, none before it reported.
    pub fn of(&self, member: &MemberId) -> Option<Capacity> {
        let capacities = self.0.lock().unwrap();
        capacities.get(member).cloned()
    }
}

impl Hub {
    /// Give each connected machine among `members` the capacity it last
    /// reported.
    pub(super) fn with_capacity(&self, members: &mut [Member]) {
        for member in members.iter_mut().filter(|member| member.connected) {
            member.capacity = self.capacities.of(&member.id);
        }
    }
}
