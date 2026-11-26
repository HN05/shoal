//! How full a pool and each of its members are, and which member a request
//! gets.
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, HashMap};

pub use shoal_protocol::resources::{
    Definition, LockMode, ResourceConfig, ResourceKind, ResourceLease, ResourceRequest, Scope,
};

/// Occupancy of one pool member.
#[derive(Default, Clone, Copy)]
pub(super) struct Usage {
    permits: u32,
    pub(super) readers: u32,
    pub(super) writers: u32,
}

impl Usage {
    /// Pool slots consumed: one per permit, one shared by all lock holders.
    pub(super) fn slots(self) -> u32 {
        self.permits + u32::from(self.readers > 0 || self.writers > 0)
    }
}

/// Request-local totals, including members removed from the current definition.
#[derive(Default)]
pub(super) struct PoolUsage<'a> {
    members: HashMap<&'a str, Usage>,
    pub(super) slots: u32,
}

impl<'a> PoolUsage<'a> {
    fn record(&mut self, lease: &'a ResourceLease) {
        let used = self.members.entry(&lease.resource).or_default();
        let before = used.slots();
        match lease.mode {
            LockMode::Permit => used.permits += 1,
            LockMode::Read => used.readers += 1,
            LockMode::Write => used.writers += 1,
        }
        self.slots += used.slots() - before;
    }

    pub(super) fn member(&self, name: &str) -> Usage {
        self.members.get(name).copied().unwrap_or_default()
    }
}

impl<'a> FromIterator<&'a ResourceLease> for PoolUsage<'a> {
    fn from_iter<T: IntoIterator<Item = &'a ResourceLease>>(leases: T) -> Self {
        let mut usage = Self::default();
        for lease in leases {
            usage.record(lease);
        }
        usage
    }
}

pub(super) fn grouped_usage<'a>(
    leases: impl IntoIterator<Item = &'a ResourceLease>,
) -> BTreeMap<(&'a Scope, &'a str), PoolUsage<'a>> {
    let mut pools = BTreeMap::<_, PoolUsage<'_>>::new();
    for lease in leases {
        pools
            .entry((&lease.scope, lease.pool.as_str()))
            .or_default()
            .record(lease);
    }
    pools
}

/// Operations on the client type [`ResourceConfig`] that only the `shoal` command uses.
pub(super) trait ResourceConfigExt {
    /// The lock mode this member grants for `requested`, or `None` when the
    /// mode does not apply to its kind.
    fn mode(&self, requested: Option<LockMode>) -> Option<LockMode>;

    fn can_acquire(&self, mode: LockMode, used: Usage, pool_available: u32) -> bool;
}

impl ResourceConfigExt for ResourceConfig {
    fn mode(&self, requested: Option<LockMode>) -> Option<LockMode> {
        match (self.kind, requested) {
            (ResourceKind::Semaphore, None | Some(LockMode::Permit)) => Some(LockMode::Permit),
            (ResourceKind::Rwlock, None | Some(LockMode::Write)) => Some(LockMode::Write),
            (ResourceKind::Rwlock, Some(LockMode::Read)) => Some(LockMode::Read),
            (ResourceKind::Repo, None | Some(LockMode::Read)) => Some(LockMode::Read),
            _ => None,
        }
    }

    fn can_acquire(&self, mode: LockMode, used: Usage, pool_available: u32) -> bool {
        match (self.kind, mode) {
            (ResourceKind::Semaphore, LockMode::Permit) => {
                used.slots() < self.capacity && pool_available > 0
            }
            (ResourceKind::Rwlock | ResourceKind::Repo, LockMode::Read) => {
                used.writers == 0 && (used.readers > 0 || pool_available > 0)
            }
            (ResourceKind::Rwlock, LockMode::Write) => used.slots() == 0 && pool_available > 0,
            _ => false,
        }
    }
}

/// Pick the least-loaded compatible member with capacity, by relative load
/// then by name.
pub(super) fn select_member<'a>(
    definition: &'a Definition,
    request: &ResourceRequest,
    usage: &PoolUsage<'_>,
    pool_available: u32,
) -> Result<Option<(&'a String, &'a ResourceConfig, LockMode)>> {
    let eligible: Vec<_> = definition
        .resources
        .iter()
        .filter(|(name, settings)| {
            request
                .resource
                .as_ref()
                .is_none_or(|wanted| wanted == *name)
                && request.kind.is_none_or(|kind| kind == settings.kind)
        })
        .filter_map(|(name, settings)| {
            let mode = settings.mode(request.mode)?;
            Some((name, settings, mode, usage.member(name)))
        })
        .collect();
    ensure!(
        !eligible.is_empty(),
        "requested kind or mode is incompatible with the selected resource(s); read/write require kind = rwlock, repo grants read only, permit requires semaphore"
    );
    let load = |slots: u32, capacity: u32| u64::from(slots) * u64::from(capacity);
    Ok(eligible
        .into_iter()
        .filter(|(_, settings, mode, used)| settings.can_acquire(*mode, *used, pool_available))
        .min_by(|a, b| {
            load(a.3.slots(), b.1.capacity)
                .cmp(&load(b.3.slots(), a.1.capacity))
                .then_with(|| a.0.cmp(b.0))
        })
        .map(|(name, settings, mode, _)| (name, settings, mode)))
}
