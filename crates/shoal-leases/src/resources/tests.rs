use super::acquire::renew_lease;
use super::catalog::pool_status;
use super::records::{LEASE_COLUMNS, leases_query, named_lease, pool_leases, row_lease};
use super::usage::{PoolUsage, grouped_usage, select_member};
use super::*;
use crate::stand_in::Fixture;
use crate::{allocation::Allocation, gates::GuardMode};
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::acquirers::Acquirer;
use shoal_store::test_support::queries::{check_owner_query, populated_database};
use std::collections::BTreeSet;
use uuid::Uuid;

mod accounting;

#[test]
fn owner_lease_reads_search_existing_index() -> Result<()> {
    check_owner_query(&leases_query(true), "resource_leases", "workspace_id")
}

#[test]
fn lease_lists_preserve_global_order_and_owner_selection() -> Result<()> {
    let db = populated_database()?;
    let all_leases = leases(&db, None)?;
    assert_eq!(all_leases.len(), 20);
    let lease_key = |lease: &ResourceLease| {
        (
            lease.scope.to_string(),
            lease.pool.clone(),
            lease.resource.clone(),
            lease.name.clone(),
            lease.id.clone(),
        )
    };
    assert!(
        all_leases
            .windows(2)
            .all(|pair| lease_key(&pair[0]) < lease_key(&pair[1]))
    );
    for owner in ["owner-0", "owner-9", "missing", ""] {
        let expected_leases: Vec<_> = all_leases
            .iter()
            .filter(|l| l.workspace_id == owner)
            .collect();
        assert_eq!(
            serde_json::to_value(leases(&db, Some(&SpaceId::from(owner)))?)?,
            serde_json::to_value(expected_leases)?
        );
    }
    Ok(())
}

#[test]
fn lease_lists_propagate_decoding_and_prepare_errors() -> Result<()> {
    let db = populated_database()?;
    db.execute_batch(
        "PRAGMA ignore_check_constraints=ON;
        UPDATE resource_leases SET mode='invalid' WHERE workspace_id='owner-0';",
    )?;
    for owner in [None, Some(SpaceId::from("owner-0"))] {
        assert!(leases(&db, owner.as_ref()).is_err());
    }
    assert_eq!(leases(&db, Some(&SpaceId::from("owner-1")))?.len(), 2);
    db.execute_batch("DROP TABLE resource_leases;")?;
    for owner in [None, Some(SpaceId::from("owner-0"))] {
        assert!(leases(&db, owner.as_ref()).is_err());
    }
    Ok(())
}

#[test]
fn scopes_keep_their_stored_spelling() -> Result<()> {
    let db = Connection::open_in_memory()?;
    for (scope, text) in [
        (Scope::Global, "global"),
        (Scope::Repo("repo-id".into()), "repo/repo-id"),
    ] {
        assert_eq!(scope.to_string(), text);
        assert_eq!(text.parse::<Scope>()?, scope);
        assert_eq!(serde_json::to_value(&scope)?, text);
        assert_eq!(serde_json::from_value::<Scope>(text.into())?, scope);
        let stored: Scope = db.query_row("SELECT ?1", [&scope], |row| row.get(0))?;
        assert_eq!(stored, scope);
        assert_eq!(
            db.query_row("SELECT ?1", [&scope], |row| row.get::<_, String>(0))?,
            text
        );
    }
    for text in ["", "repo", "repo/", "local", "Global"] {
        assert!(text.parse::<Scope>().is_err(), "{text}");
        assert!(
            db.query_row("SELECT ?1", [text], |row| row.get::<_, Scope>(0))
                .is_err(),
            "{text}"
        );
    }
    Ok(())
}

#[test]
fn leases_use_named_columns() -> Result<()> {
    let db = Connection::open_in_memory()?;
    // Deliberately use a physical order different from LEASE_COLUMNS.
    db.execute_batch(
        "CREATE TABLE repository_resource_leases (lease_id TEXT, repository_id TEXT, path TEXT);
            CREATE TABLE resource_leases AS SELECT
            'space' AS acquirer_id, 'agent' AS acquirer, 'read' AS mode, 123 AS created_at, NULL AS reason, 'member' AS resource,
            'default' AS name, 'pool' AS pool, 'global' AS scope,
            'workspace' AS workspace_id, 'lease' AS id;",
    )?;
    let expected = serde_json::json!({
        "id": "lease", "workspace_id": "workspace", "scope": "global", "pool": "pool",
        "name": "default", "resource": "member", "reason": null, "created_at": 123, "mode": "read",
        "acquirer": {"kind": "agent", "space_id": "space"}
    });
    assert_eq!(serde_json::to_value(&leases(&db, None)?[0])?, expected);
    assert_eq!(leases(&db, Some(&SpaceId::from("workspace")))?.len(), 1);
    assert!(leases(&db, Some(&SpaceId::from("other")))?.is_empty());
    let reordered = LEASE_COLUMNS.split(',').rev().collect::<Vec<_>>().join(",");
    let lease = db.query_row(
        &format!("SELECT {reordered} FROM resource_leases"),
        [],
        row_lease,
    )?;
    assert_eq!(serde_json::to_value(lease)?, expected);
    Ok(())
}

#[test]
fn allocation_reads_only_the_named_lease_and_selected_pool() -> Result<()> {
    let mut db = shoal_store::test_support::migrated()?;
    db.execute_batch(
            "INSERT INTO repositories(id,path,source,last_used) VALUES ('a','/a','a',0),('b','/b','b',0);
             INSERT INTO spaces(id,kind,repository_id,name) VALUES
             ('a','workspace','a','a'),('b','workspace','b','b');
             INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir) VALUES
             ('a','/a/w','a','ready','abc','/admin'),('b','/b/w','b','ready','abc','/admin');
             INSERT INTO resource_pools VALUES ('global','target','{}'),('repo/a','target','{}'),
             ('repo/b','target','{}'),('global','other','{}');",
        )?;
    for (id, owner, scope, pool, member, mode) in [
        ("one", "a", "global", "target", "z", "permit"),
        ("two", "b", "global", "target", "a", "read"),
        ("three", "a", "repo/a", "target", "a", "write"),
        ("four", "b", "repo/b", "target", "a", "read"),
        ("five", "a", "global", "other", "a", "permit"),
    ] {
        db.execute(
            "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at,mode)
                VALUES (?1,?2,?3,?4,?1,?5,0,?6)",
            params![id, owner, scope, pool, member, mode],
        )?;
    }
    let tx = db.transaction()?;
    // Leases of another pool are left out of the selection.
    for i in 0..100 {
        tx.execute(
            "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at,mode)
                VALUES (?1,'b','global','other',?1,'a',0,'permit')",
            [format!("noise-{i}")],
        )?;
    }
    tx.commit()?;
    let all = leases(&db, None)?;
    for scope in [
        Scope::Global,
        Scope::Repo("a".into()),
        Scope::Repo("b".into()),
    ] {
        let selected = pool_leases(&db, &scope, "target")?;
        let expected: Vec<_> = all
            .iter()
            .filter(|l| l.scope == scope && l.pool == "target")
            .collect();
        assert_eq!(
            serde_json::to_value(&selected)?,
            serde_json::to_value(expected)?
        );
    }
    let selected = pool_leases(&db, &Scope::Global, "target")?;
    assert_eq!(selected.len(), 2);
    db.execute(
        "UPDATE resource_leases SET acquirer='agent',acquirer_id='agent' WHERE id='three'",
        [],
    )?;
    let agent = Acquirer::Agent {
        space_id: "agent".into(),
    };
    let existing = named_lease(&db, &SpaceId::from("a"), "target", "three")?.unwrap();
    assert_eq!(existing.scope, Scope::Repo("a".into()));
    assert!(named_lease(&db, &SpaceId::from("b"), "target", "three")?.is_none());
    assert!(named_lease(&db, &SpaceId::from("a"), "other", "three")?.is_none());
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "target".into(),
        name: "three".into(),
        resource: None,
        reason: Some("renewed".into()),
        queue: false,
    };
    // A new global definition cannot hide a repository-scoped name.
    let renewed = renew_lease(
        db.transaction()?,
        &existing,
        &Scope::Global,
        request.clone(),
        &Acquirer::User,
    );
    assert!(renewed.is_err());
    // Another agent's acquire leaves the lease the agent's.
    let other = Acquirer::Agent {
        space_id: "other".into(),
    };
    renew_lease(
        db.transaction()?,
        &existing,
        &existing.scope,
        request.clone(),
        &other,
    )?;
    let renewed = named_lease(&db, &SpaceId::from("a"), "target", "three")?.unwrap();
    assert_eq!(renewed.id, existing.id);
    assert_eq!(renewed.reason.as_deref(), Some("renewed"));
    assert_eq!(renewed.acquirer, agent);
    // The user's makes it the user's.
    renew_lease(
        db.transaction()?,
        &renewed,
        &renewed.scope,
        request,
        &Acquirer::User,
    )?;
    let taken = named_lease(&db, &SpaceId::from("a"), "target", "three")?.unwrap();
    assert_eq!(taken.acquirer, Acquirer::User);
    Ok(())
}

#[test]
fn config_defaults_sum_member_capacities_and_reject_invalid_definitions() {
    let config: crate::config::repo::RepoConfig = toml::from_str("[resources.lock]\n[resource_pools.workers.resources.a]\n[resource_pools.workers.resources.b]\ncapacity=2\n").unwrap();
    let normalized = definitions(&config.resources, &config.resource_pools).unwrap();
    assert_eq!(normalized["lock"].capacity, 1);
    assert_eq!(normalized["workers"].capacity, 3);
    for text in [
        "[resources.lock]\ncapacity=0",
        "[resource_pools.empty]\ncapacity=2",
        "[resources.lock]\n[resource_pools.lock.resources.a]",
        "[resources.'invalid/name']",
        "[resource_pools.p.resources.a]\ncapacity=65536",
        "[resources.x]\nreason='  '",
        "[resources.cache]\nkind='rwlock'\ncapacity=2",
        "[resources.server]\nkind='repo'",
        "[resources.server]\nkind='repo'\nrepo=''",
        "[resources.server]\nkind='repo'\nrepo='../server'",
        "[resources.server]\nkind='repo'\nrepo='https://'",
        "[resources.server]\nkind='repo'\nrepo='server'\ncapacity=2",
        "[resources.server]\nrepo='server'",
    ] {
        let config: crate::config::repo::RepoConfig = toml::from_str(text).unwrap();
        assert!(
            definitions(&config.resources, &config.resource_pools).is_err(),
            "{text}"
        );
    }
    assert!(toml::from_str::<crate::config::repo::RepoConfig>("[resources.x]\ncapcity=2").is_err());
    assert!(
        toml::from_str::<crate::config::repo::RepoConfig>("[resources.x]\nkind='unknown'").is_err()
    );
}

#[test]
fn repository_resources_validate_and_layer_by_name() -> Result<()> {
    let base = crate::config::repo::parse(
        "[resources.server]\nkind='repo'\nrepo='server'\nrequires_approval=true",
    )?;
    let local = crate::config::repo::parse("[resources.server]\nkind='repo'\nrepo='other-server'")?;
    let merged = local.over(base);
    assert_eq!(
        merged.resources["server"].repo.as_deref(),
        Some("other-server")
    );
    assert!(!merged.resources["server"].requires_approval);
    for url in [
        "https://example.test/team/server.git",
        "git@example.test:team/server.git",
    ] {
        let config =
            crate::config::repo::parse(&format!("[resources.server]\nkind='repo'\nrepo='{url}'"))?;
        definitions(&config.resources, &config.resource_pools)?;
    }
    let json = serde_json::to_value(&merged.resources["server"])?;
    let restored: ResourceConfig = serde_json::from_value(json)?;
    assert_eq!(restored, merged.resources["server"]);
    Ok(())
}

#[tokio::test]
async fn queued_requests_take_freed_capacity_in_arrival_order() {
    let fixture = Fixture::new("[resources.lock]\n", "").await;
    let (leases, daemon) = (&fixture.leases, &fixture.daemon);
    let selector = fixture.selector();
    let request = |name: &str, queue: bool| ResourceRequest {
        mode: None,
        kind: None,
        pool: "lock".into(),
        name: name.into(),
        resource: None,
        reason: None,
        queue,
    };
    let acquire = async |name: &str, queue: bool| {
        let acquired = leases.acquire_resource(daemon, &selector, request(name, queue), None);
        match acquired.await {
            Ok(Allocation::Granted(lease)) => Ok(lease.name),
            Ok(Allocation::Busy(message)) => Err(message),
            Ok(Allocation::Approval(_)) => panic!("no approval configured"),
            Err(error) => panic!("{error:#}"),
        }
    };
    let hook = (leases.gates)
        .resource_guard(&fixture.workspace.id, GuardMode::Exclusive)
        .unwrap();
    assert!(acquire("held", true).await.is_err());
    let plain = leases.acquire_resource(daemon, &selector, request("plain", false), None);
    assert!(plain.await.is_err());
    drop(hook);
    assert_eq!(acquire("held", true).await.unwrap(), "held");
    let busy = acquire("first", true).await.unwrap_err();
    assert!(busy.starts_with("no compatible"), "{busy}");
    assert!(busy.ends_with("; held by worker"), "{busy}");
    let queued = acquire("second", true).await.unwrap_err();
    assert!(queued.starts_with("1 earlier"), "{queued}");
    (leases.release_resource(daemon, &selector, "lock".into(), "held".into()))
        .await
        .unwrap();
    let late = acquire("late", false).await.unwrap_err();
    assert!(
        late.starts_with("2 earlier"),
        "free capacity goes to the queue: {late}"
    );
    let queued = acquire("second", true).await.unwrap_err();
    assert!(queued.starts_with("1 earlier"), "{queued}");
    assert_eq!(acquire("first", true).await.unwrap(), "first");
    assert_eq!(acquire("first", false).await.unwrap(), "first", "renewal");
    assert!(
        acquire("second", true)
            .await
            .unwrap_err()
            .starts_with("no compatible")
    );
}

/// A global resource that takes another's definition stays global, as the
/// workspace's settings resolve it, rather than conflicting with it.
#[tokio::test]
async fn a_global_resource_takes_another_ones_definition() {
    let global = "[resources]\nlock = { capacity = 2 }\ncopy = { same_as = \"lock\" }\n";
    let fixture = Fixture::new(global, "").await;
    let definitions = (fixture.leases)
        .resource_definitions(&fixture.daemon, &fixture.workspace)
        .await
        .unwrap();
    let (scope, copy) = &definitions["copy"];
    assert_eq!(*scope, Scope::Global);
    assert_eq!(copy.capacity, 2);
}
