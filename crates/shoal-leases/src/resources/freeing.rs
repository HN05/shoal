//! The held leases whose release would grant a request that capacity keeps
//! busy: one lease, or every lease on one member, since readers share a
//! member's slot and a writer needs all of them gone.
use std::collections::BTreeMap;

use super::{Definition, PoolUsage, ResourceLease, ResourceRequest, select_member};

/// Each set of the pool's `active` leases whose release would let `request`
/// acquire a member of `definition`: each lease that would alone, then each
/// member's leases together where none of them would alone.
pub(super) fn freeing(
    definition: &Definition,
    request: &ResourceRequest,
    active: &[ResourceLease],
) -> Vec<Vec<ResourceLease>> {
    let grants = |released: &[&ResourceLease]| grants(definition, request, active, released);
    let singles: Vec<&ResourceLease> = (active.iter()).filter(|lease| grants(&[lease])).collect();
    let mut members: BTreeMap<&str, Vec<&ResourceLease>> = BTreeMap::new();
    for lease in active {
        members.entry(&lease.resource).or_default().push(lease);
    }
    let shared = (members.into_values()).filter(|leases| {
        leases.len() > 1
            && leases
                .iter()
                .all(|lease| !singles.iter().any(|single| single.id == lease.id))
            && grants(leases)
    });
    (singles.iter().map(|lease| vec![*lease]))
        .chain(shared)
        .map(|released| released.into_iter().cloned().collect())
        .collect()
}

/// Whether `request` could acquire a member of `definition` once
/// `released`, among `active`, went.
fn grants(
    definition: &Definition,
    request: &ResourceRequest,
    active: &[ResourceLease],
    released: &[&ResourceLease],
) -> bool {
    let left: PoolUsage<'_> = (active.iter())
        .filter(|lease| released.iter().all(|gone| gone.id != lease.id))
        .collect();
    let available = definition.capacity.saturating_sub(left.slots);
    matches!(
        select_member(definition, request, &left, available),
        Ok(Some(_))
    )
}

#[cfg(test)]
mod tests;
