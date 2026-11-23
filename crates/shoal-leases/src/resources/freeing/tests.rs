use super::*;
use crate::time::UnixTime;
use shoal_protocol::acquirers::Acquirer;
use shoal_protocol::resources::{LockMode, ResourceConfig, ResourceKind, Scope};

/// A pool of `capacity` with each of `members` as `(name, kind, capacity)`.
fn pool(capacity: u32, members: &[(&str, ResourceKind, u32)]) -> Definition {
    let resources = (members.iter())
        .map(|&(name, kind, capacity)| {
            let config = ResourceConfig {
                kind,
                capacity,
                ..ResourceConfig::default()
            };
            (name.to_owned(), config)
        })
        .collect();
    Definition {
        capacity,
        reason: None,
        resources,
    }
}

/// The lease `id` of `member` in `mode`.
fn lease(id: &str, member: &str, mode: LockMode) -> ResourceLease {
    ResourceLease {
        id: id.into(),
        workspace_id: "workspace".into(),
        scope: Scope::Global,
        pool: "pool".into(),
        name: id.into(),
        resource: member.into(),
        reason: None,
        created_at: UnixTime::from_seconds(1),
        mode,
        repository: None,
        acquirer: Acquirer::User,
    }
}

fn request(member: Option<&str>, mode: Option<LockMode>) -> ResourceRequest {
    ResourceRequest {
        pool: "pool".into(),
        name: "new".into(),
        resource: member.map(Into::into),
        mode,
        ..ResourceRequest::default()
    }
}

/// The IDs of each set of leases that frees the request.
fn ids(freeing: Vec<Vec<ResourceLease>>) -> Vec<Vec<String>> {
    (freeing.into_iter())
        .map(|leases| leases.into_iter().map(|lease| lease.id).collect())
        .collect()
}

#[test]
fn a_full_member_frees_with_any_of_its_permits() {
    let definition = pool(4, &[("a", ResourceKind::Semaphore, 2)]);
    let active = [
        lease("one", "a", LockMode::Permit),
        lease("two", "a", LockMode::Permit),
    ];

    let freeing = freeing(&definition, &request(None, None), &active);

    assert_eq!(ids(freeing), [vec!["one"], vec!["two"]]);
}

#[test]
fn a_full_pool_frees_with_a_permit_of_any_member() {
    let semaphore = ResourceKind::Semaphore;
    let definition = pool(2, &[("a", semaphore, 2), ("b", semaphore, 2)]);
    let active = [
        lease("on-a", "a", LockMode::Permit),
        lease("on-b", "b", LockMode::Permit),
    ];

    let freeing = freeing(&definition, &request(Some("a"), None), &active);

    assert_eq!(ids(freeing), [vec!["on-a"], vec!["on-b"]]);
}

#[test]
fn a_writer_frees_only_with_every_reader_in_its_way() {
    let definition = pool(2, &[("lock", ResourceKind::Rwlock, 1)]);
    let active = [
        lease("first", "lock", LockMode::Read),
        lease("second", "lock", LockMode::Read),
    ];

    let freeing = freeing(&definition, &request(None, Some(LockMode::Write)), &active);

    assert_eq!(ids(freeing), [vec!["first", "second"]]);
}
