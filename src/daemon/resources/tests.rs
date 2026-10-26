use super::*;

#[test]
fn owner_lease_reads_search_existing_index() -> Result<()> {
    store::query_tests::check_owner_query(&leases_query(true), "resource_leases", "workspace_id")
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
            'read' AS mode, 123 AS created_at, NULL AS reason, 'member' AS resource,
            'default' AS name, 'pool' AS pool, 'global' AS scope,
            'workspace' AS workspace_id, 'lease' AS id;",
    )?;
    let expected = serde_json::json!({
        "id": "lease", "workspace_id": "workspace", "scope": "global", "pool": "pool",
        "name": "default", "resource": "member", "reason": null, "created_at": 123, "mode": "read"
    });
    assert_eq!(serde_json::to_value(&leases(&db, None)?[0])?, expected);
    assert_eq!(leases(&db, Some("workspace"))?.len(), 1);
    assert!(leases(&db, Some("other"))?.is_empty());
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
    let mut db = Connection::open_in_memory()?;
    db.execute_batch(include_str!("../../../tests/fixtures/schema_v17.sql"))?;
    db.execute_batch(
        "CREATE TABLE repository_resource_leases (lease_id TEXT, repository_id TEXT, path TEXT);",
    )?;
    db.execute_batch(
            "INSERT INTO repositories(id,path,source,last_used) VALUES ('a','/a','a',0),('b','/b','b',0);
             INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES
             ('a','a','a','/a/w','a','ready'),('b','b','b','/b/w','b','ready');
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
            "INSERT INTO resource_leases VALUES (?1,?2,?3,?4,?1,?5,NULL,0,?6)",
            params![id, owner, scope, pool, member, mode],
        )?;
    }
    for unrelated in [0, 100, 10_000] {
        db.execute("DELETE FROM resource_leases WHERE name LIKE 'noise-%'", [])?;
        let tx = db.transaction()?;
        for i in 0..unrelated {
            tx.execute(
                    "INSERT INTO resource_leases VALUES (?1,'b','global','other',?1,'a',NULL,0,'permit')",
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
        let existing = named_lease(&db, "a", "target", "three")?.unwrap();
        assert_eq!(existing.scope, Scope::Repo("a".into()));
        assert!(named_lease(&db, "b", "target", "three")?.is_none());
        assert!(named_lease(&db, "a", "other", "three")?.is_none());
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
        let error = renew_lease(
            db.transaction()?,
            &existing,
            &Scope::Global,
            request.clone(),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("different settings"));
        renew_lease(db.transaction()?, &existing, &existing.scope, request)?;
        let renewed = named_lease(&db, "a", "target", "three")?.unwrap();
        assert_eq!(renewed.id, existing.id);
        assert_eq!(renewed.reason.as_deref(), Some("renewed"));
        eprintln!(
            "unrelated={unrelated}: global rows={}, candidate rows={}, renewal rows=1",
            all.len(),
            selected.len()
        );
    }
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
    use crate::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    crate::test_support::configure(&manager, "[resources.lock]\n").await;
    let repository_dir = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(repository_dir.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap()
        .id;
    let acquire = async |name: &str, queue: bool| {
        let request = ResourceRequest {
            mode: None,
            kind: None,
            pool: "lock".into(),
            name: name.into(),
            resource: None,
            reason: None,
            queue,
        };
        match manager.acquire_resource(&workspace, request, None).await {
            Ok(Allocation::Granted(lease)) => Ok(lease.name),
            Ok(Allocation::Busy(message)) => Err(message),
            Ok(Allocation::Approval(_)) => panic!("no approval configured"),
            Err(error) => panic!("{error:#}"),
        }
    };
    let hook = manager
        .resource_guard(&workspace, crate::daemon::workspace::GuardMode::Exclusive)
        .await
        .unwrap();
    let in_progress = "workspace resource operation is in progress; retry when its hook finishes";
    assert_eq!(acquire("held", true).await.unwrap_err(), in_progress);
    let request = ResourceRequest {
        mode: None,
        kind: None,
        pool: "lock".into(),
        name: "plain".into(),
        resource: None,
        reason: None,
        queue: false,
    };
    let error = manager.acquire_resource(&workspace, request, None).await;
    assert_eq!(error.err().unwrap().to_string(), in_progress);
    drop(hook);
    assert_eq!(acquire("held", true).await.unwrap(), "held");
    assert!(
        acquire("first", true)
            .await
            .unwrap_err()
            .starts_with("no compatible")
    );
    assert_eq!(
        acquire("second", true).await.unwrap_err(),
        "1 earlier request is queued for pool lock"
    );
    manager
        .release_resource(&workspace, "lock".into(), "held".into())
        .await
        .unwrap();
    assert_eq!(
        acquire("late", false).await.unwrap_err(),
        "2 earlier requests are queued for pool lock",
        "free capacity goes to the queue"
    );
    assert_eq!(
        acquire("second", true).await.unwrap_err(),
        "1 earlier request is queued for pool lock"
    );
    assert_eq!(acquire("first", true).await.unwrap(), "first");
    assert_eq!(acquire("first", false).await.unwrap(), "first", "renewal");
    assert!(
        acquire("second", true)
            .await
            .unwrap_err()
            .starts_with("no compatible")
    );
}
