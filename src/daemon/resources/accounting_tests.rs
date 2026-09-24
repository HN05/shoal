use super::*;

fn lease(scope: Scope, pool: &str, resource: &str, mode: LockMode) -> ResourceLease {
    ResourceLease {
        mode,
        id: Uuid::new_v4().to_string(),
        workspace_id: "owner".into(),
        scope,
        pool: pool.into(),
        name: "default".into(),
        resource: resource.into(),
        reason: None,
        created_at: 0,
    }
}

#[test]
fn grouped_accounting_visits_each_lease_once_as_members_grow() {
    for member_count in [1, 10, 100, 1_000] {
        let definition = Definition {
            capacity: 500,
            reason: None,
            resources: (0..member_count)
                .map(|i| (format!("member-{i:04}"), ResourceConfig::default()))
                .collect(),
        };
        let mut leases = Vec::new();
        for scope in [
            Scope::Global,
            Scope::Repo("a".into()),
            Scope::Repo("b".into()),
        ] {
            for pool in ["target", "other"] {
                for i in 0..240 {
                    let resource = format!("member-{:04}", i % member_count);
                    let mode = [LockMode::Permit, LockMode::Read, LockMode::Write][i % 3];
                    leases.push(lease(scope.clone(), pool, &resource, mode));
                }
                // Drift must still charge capacity for a removed member.
                leases.push(lease(scope.clone(), pool, "removed", LockMode::Read));
            }
        }
        let mut visits = 0;
        let grouped = grouped_usage(leases.iter().inspect(|_| visits += 1));
        assert_eq!(visits, leases.len());
        assert_eq!(grouped.len(), 6);
        let mut reference_visits = 0;
        for ((scope, pool), usage) in grouped {
            let active: Vec<_> = leases
                .iter()
                .filter(|l| &l.scope == scope && l.pool == pool)
                .collect();
            let permits = active.iter().filter(|l| l.mode == LockMode::Permit).count();
            let locks: BTreeSet<_> = active
                .iter()
                .filter(|l| l.mode != LockMode::Permit)
                .map(|l| &l.resource)
                .collect();
            assert_eq!(usage.slots as usize, permits + locks.len());
            // Compare against the former per-member scan, including mixed stored modes.
            let status = pool_status(pool.into(), scope.clone(), &definition, &usage, true);
            assert_eq!(status.used, usage.slots);
            assert!(
                status
                    .resources
                    .windows(2)
                    .all(|pair| pair[0].name < pair[1].name)
            );
            for member in status.resources {
                let matching: Vec<_> = active
                    .iter()
                    .inspect(|_| reference_visits += 1)
                    .filter(|l| l.resource == member.name)
                    .collect();
                let permits = matching
                    .iter()
                    .filter(|l| l.mode == LockMode::Permit)
                    .count() as u32;
                let readers = matching.iter().filter(|l| l.mode == LockMode::Read).count() as u32;
                let writers = matching
                    .iter()
                    .filter(|l| l.mode == LockMode::Write)
                    .count() as u32;
                assert_eq!((member.readers, member.writers), (readers, writers));
                assert_eq!(member.used, permits + u32::from(readers + writers > 0));
                assert_eq!(
                    member.available,
                    1u32.saturating_sub(member.used).min(500 - usage.slots)
                );
            }
            let drifted = pool_status(pool.into(), scope.clone(), &definition, &usage, false);
            assert_eq!(drifted.used, usage.slots);
            assert_eq!(drifted.available, 0);
            assert!(
                drifted
                    .resources
                    .iter()
                    .all(|r| !r.read_available && !r.write_available)
            );
        }
        assert_eq!(reference_visits, leases.len() * member_count);
        eprintln!(
            "members={member_count}: aggregate visits={visits}, former member scan visits={reference_visits}"
        );
    }
}

#[test]
fn selection_preserves_relative_load_name_ties_and_shared_readers() -> Result<()> {
    let definition: Definition = toml::from_str(
        "capacity=4\n[resources.alpha]\ncapacity=2\n[resources.beta]\ncapacity=4\n[resources.cache]\nkind='rwlock'\n",
    )?;
    let mut active = vec![
        lease(Scope::Global, "pool", "alpha", LockMode::Permit),
        lease(Scope::Global, "pool", "beta", LockMode::Permit),
        lease(Scope::Global, "pool", "cache", LockMode::Read),
        lease(Scope::Global, "pool", "cache", LockMode::Read),
    ];
    let mut request = ResourceRequest {
        mode: Some(LockMode::Permit),
        pool: "pool".into(),
        name: "next".into(),
        resource: None,
        reason: None,
    };
    let usage = active.iter().collect::<PoolUsage<'_>>();
    assert_eq!(
        select_member(&definition, &request, &usage, 1)?.unwrap().0,
        "beta"
    );
    active.push(lease(Scope::Global, "pool", "beta", LockMode::Permit));
    let usage = active.iter().collect::<PoolUsage<'_>>();
    assert_eq!(usage.slots, 4);
    assert_eq!(
        select_member(&definition, &request, &usage, 1)?.unwrap().0,
        "alpha"
    );
    assert!(select_member(&definition, &request, &usage, 0)?.is_none());
    request.mode = Some(LockMode::Read);
    assert_eq!(
        select_member(&definition, &request, &usage, 0)?.unwrap().0,
        "cache"
    );
    request.mode = Some(LockMode::Write);
    assert!(select_member(&definition, &request, &usage, 1)?.is_none());
    let status = pool_status("pool".into(), Scope::Global, &definition, &usage, true);
    assert_eq!(status.used, 4);
    assert!(status.resources[2].read_available);
    assert!(!status.resources[2].write_available);
    assert_eq!(status.resources[2].readers, 2);
    Ok(())
}
