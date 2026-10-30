use super::*;

fn resource(mode: LockMode) -> Specification {
    let definition = Definition {
        capacity: 1,
        reason: None,
        resources: std::collections::BTreeMap::from([(
            "lock".to_owned(),
            crate::daemon::resources::ResourceConfig::default(),
        )]),
    };
    Specification::Resource(ResourceSpecification {
        repository: None,
        definition,
        member: "lock".into(),
        mode,
    })
}

fn owners(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
            INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES
                ('owner','repo','worker','/work','worker','ready'),
                ('other','repo','other','/other','other','ready');",
    )?;
    Ok(())
}

/// Store `request` under its key, keeping the number a stored request
/// with that key holds or taking a free one.
fn save(db: &Connection, request: &ApprovalRequest) -> Result<()> {
    let number = match db
        .query_row(
            "SELECT number FROM access_requests WHERE id=?1",
            [&request.key],
            |r| r.get(0),
        )
        .optional()?
    {
        Some(number) => number,
        None => store::free_number(db, "access_requests")?,
    };
    db.execute(
            "INSERT OR REPLACE INTO access_requests(id,workspace_id,target_key,name,record,active,number)
            VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                request.key,
                request.workspace_id,
                request.target,
                request.name,
                serde_json::to_string(request)?,
                request.active,
                number
            ],
        )?;
    Ok(())
}

/// The number stored under `key`.
fn number(db: &Connection, key: &str) -> Result<i64> {
    Ok(db.query_row(
        "SELECT number FROM access_requests WHERE id=?1",
        [key],
        |r| r.get(0),
    )?)
}

fn sample(owner: &str, target: Target) -> ApprovalRequest {
    ApprovalRequest::new(
        owner,
        target,
        "default",
        resource(LockMode::Read),
        Lifetime::Workspace,
        Some("read shared data"),
    )
}

#[tokio::test]
async fn current_lookup_isolates_names_and_ignores_unrelated_history() -> Result<()> {
    let (_root, manager) = crate::test_support::manager().await;
    manager.store.run(|db| {
            owners(db)?;
            let target = Target::Resource { scope: Scope::Global, pool: "lock".into() };
            for (id, owner, target) in [
                ("wanted", "owner", target.clone()),
                ("other-owner", "other", target.clone()),
                ("other-target", "owner", Target::Resource { scope: Scope::Global, pool: "other".into() }),
            ] {
                let mut request = sample(owner, target.clone());
                request.key = id.into();
                save(db, &request)?;
                assert_eq!(current(db, owner, &target, "default")?.unwrap().key, id);
            }
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('history','owner',?1,'default','invalid',0,100)", [&target])?;
            assert!(list(db, Some("owner")).is_err());
            assert_eq!(current(db, "owner", &target, "default")?.unwrap().key, "wanted");
            assert!(current(db, "owner", &target, "absent")?.is_none());
            let tx = db.transaction()?;
            let request = sample("owner", target.clone());
            assert_eq!(check(&tx, request.clone())?.unwrap().key, "wanted");
            let mut changed = request.clone();
            changed.lifetime = Lifetime::Lease;
            assert!(check(&tx, changed).is_err());
            let mut changed = request;
            changed.reason = "different reason".into();
            assert!(check(&tx, changed).is_err());
            tx.execute("UPDATE access_requests SET record='invalid' WHERE id='wanted'", [])?;
            assert!(current(&tx, "owner", &target, "default").is_err());
            Ok(())
        }).await
}

#[tokio::test]
async fn decisions_decode_only_the_selected_id_and_remain_idempotent() -> Result<()> {
    let (_root, manager) = crate::test_support::manager().await;
    manager.store.run(|db| {
            owners(db)?;
            for key in ["approve", "deny"] {
                let mut request = sample("owner", Target::Simulator);
                request.key = key.into();
                request.name = key.into();
                save(db, &request)?;
            }
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('broken','other','simulator','default','invalid',0,3)", [])?;
            Ok(())
        }).await?;
    for (id, approve, status) in [
        (1, true, DecisionStatus::Approved),
        (2, false, DecisionStatus::Denied),
    ] {
        let decision = manager
            .decide_approval(id, approve, None, Default::default())
            .await?;
        assert_eq!(decision.status, status);
        let messages = manager.agent_messages("worker").await?;
        let ids: Vec<_> = messages.iter().map(|m| m.id).collect();
        assert_eq!(
            messages.into_iter().map(|m| m.message).collect::<Vec<_>>(),
            [decision.decision_message()]
        );
        manager.mark_agent_messages_delivered("worker", ids).await?;
        let retry = manager
            .decide_approval(id, approve, None, Default::default())
            .await?;
        assert_eq!(
            serde_json::to_value(retry)?,
            serde_json::to_value(&decision)?
        );
        // Repeating a decision does not tell the agent again.
        assert!(manager.agent_messages("worker").await?.is_empty());
        assert!(
            manager
                .decide_approval(id, !approve, None, Default::default())
                .await
                .is_err()
        );
    }
    // A decision chosen from a shown request does not reach another
    // request holding its number.
    let shown = manager.approval_requests(Some("worker")).await?;
    let error = manager
        .decide_approval(1, true, Some(shown[1].clone()), Default::default())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("changed since it was shown"),
        "{error:#}"
    );
    // Displayed snapshots travel through JSON to the CLI and back.
    let snapshot: ApprovalRequest = serde_json::from_value(serde_json::to_value(&shown[0])?)?;
    let mut replaced = snapshot.clone();
    replaced.key = "a different request with the same visible details".into();
    let error = manager
        .decide_approval(1, true, Some(replaced), Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("changed since it was shown"));
    manager
        .decide_approval(1, true, Some(snapshot), Default::default())
        .await?;
    for (id, message) in [
        (4, "unknown approval request 4"),
        (3, "approval request 3 has an invalid record"),
    ] {
        assert!(
            manager
                .decide_approval(id, true, None, Default::default())
                .await
                .unwrap_err()
                .to_string()
                .contains(message)
        );
    }
    manager
        .store
        .run(|db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id='owner'", [])?;
            Ok(())
        })
        .await?;
    assert!(
        manager
            .decide_approval(1, true, None, Default::default())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn replaced_land_requests_keep_their_numbers_and_refuse_decisions() -> Result<()> {
    let (_root, manager) = crate::test_support::manager().await;
    manager
        .store
        .run(|db| {
            owners(db)?;
            let tx = db.transaction()?;
            let mut requests = Vec::new();
            for head in ["first", "second", "third"] {
                let request = ApprovalRequest::new(
                    "owner",
                    Target::Land,
                    LAND,
                    Specification::Land(LandSpecification {
                        branch: "feature".into(),
                        head: head.into(),
                    }),
                    Lifetime::Lease,
                    Some("land it"),
                );
                requests.push(request_land(&tx, request)?.unwrap());
            }
            let numbers: Vec<_> = requests.iter().map(|request| request.id).collect();
            assert_eq!(numbers, [1, 2, 3]);
            for outdated in &requests[..2] {
                let error = decide(&tx, &outdated.key, true).err().unwrap();
                assert!(error.to_string().contains("is outdated"), "{error:#}");
                let stored = by_key(&tx, &outdated.key)?.unwrap();
                assert_eq!(stored.status, DecisionStatus::Outdated);
                assert!(!stored.active);
            }
            assert!(decide(&tx, &requests[2].key, true)?.new);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn withdrawn_requests_keep_their_numbers_and_take_back_their_notifications() {
    let (root, manager) = crate::test_support::manager().await;
    let path = crate::test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let request = async || {
        let request = manager
            .request_approval(sample(&workspace.id, Target::Port("web".into())))
            .await
            .unwrap()
            .unwrap();
        manager.notify_approval(&workspace.name, &request).await;
        request
    };
    let unseen = request().await;
    let error = manager
        .withdraw_approval(&other.name, unseen.id)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("has no approval request"),
        "{error:#}"
    );

    let withdrawn = manager
        .withdraw_approval(&workspace.name, unseen.id)
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
    assert!(!withdrawn.active);
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
    let default = proposals::ProposalDecision::default;
    for error in [
        manager
            .decide_approval(unseen.id, true, None, default())
            .await
            .unwrap_err(),
        manager
            .withdraw_approval(&workspace.name, unseen.id)
            .await
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("is withdrawn"), "{error:#}");
    }

    // Requesting again asks anew under another number; once the user saw
    // that one, withdrawing it tells them.
    let shown = request().await;
    assert_ne!(shown.id, unseen.id);
    let notifications = manager.notifications(true, 50).await.unwrap();
    manager
        .mark_notifications_read(notifications.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    manager
        .withdraw_approval(&workspace.name, shown.id)
        .await
        .unwrap();
    let unread = manager.notifications(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].kind, NotificationKind::ApprovalWithdrawn);
    assert_eq!(
        unread[0].message,
        format!(
            "approval {}: port web (workspace) was withdrawn; no decision needed",
            shown.id
        )
    );

    // Workspace processes withdraw only their own requests.
    let caller = crate::daemon::scope::Caller {
        workspace_id: workspace.id.clone(),
        execution: None,
    };
    manager.executions.issue_scope("scope".into(), caller).await;
    for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
        let mut method = crate::protocol::ApprovalMethod::WithdrawApproval {
            workspace: target.clone(),
            id: shown.id,
        };
        let authorized = crate::daemon::scope::authorize(&manager, Some("scope"), &mut method)
            .await
            .is_ok();
        assert_eq!(authorized, allowed);
    }
}

#[tokio::test]
async fn a_withdrawn_request_is_never_announced_afterwards() {
    let (root, manager) = crate::test_support::manager().await;
    let path = crate::test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    let request = manager
        .request_approval(sample(&workspace.id, Target::Port("web".into())))
        .await
        .unwrap()
        .unwrap();
    manager
        .withdraw_approval(&workspace.name, request.id)
        .await
        .unwrap();
    // The allocation announces the request it recorded only now.
    manager.notify_approval(&workspace.name, &request).await;
    let unread = manager.notifications(true, 50).await.unwrap();
    assert!(
        unread
            .iter()
            .all(|n| n.kind != NotificationKind::ApprovalRequested),
        "{unread:?}"
    );
}

#[tokio::test]
async fn withdrawal_never_reaches_a_request_that_took_the_number_meanwhile() -> Result<()> {
    let (_root, manager) = crate::test_support::manager().await;
    manager
        .store
        .run(|db| {
            owners(db)?;
            let tx = db.transaction()?;
            let selected = check(&tx, sample("owner", Target::Port("web".into())))?.unwrap();
            // Decided and gone while the withdrawal waited for its gate.
            release(&tx, "owner", &Target::Port("web".into()), "default")?;
            let later = check(&tx, sample("owner", Target::Port("api".into())))?.unwrap();
            assert_eq!(later.id, selected.id);
            let error = withdraw(&tx, &selected).unwrap_err();
            assert!(error.to_string().contains("decided meanwhile"), "{error:#}");
            let stored = by_key(&tx, &later.key)?.unwrap();
            assert_eq!(stored.status, DecisionStatus::Pending);
            Ok(())
        })
        .await
}

#[test]
fn decision_messages_name_the_request_target_and_next_step() {
    let mut request = sample("owner", Target::Port("web".into()));
    request.id = 7;
    request.status = DecisionStatus::Approved;
    assert_eq!(
        request.decision_message(),
        "Approval 7 for port web (workspace) was approved: retry the command that requested it."
    );
    request.status = DecisionStatus::Denied;
    assert_eq!(
        request.decision_message(),
        "Approval 7 for port web (workspace) was denied: do not retry it."
    );
    request.target = Target::Land;
    request.specification = Specification::Land(LandSpecification {
        branch: "feature".into(),
        head: "abc".into(),
    });
    assert_eq!(
        request.decision_message(),
        "Approval 7 for land feature at abc was denied: do not retry it."
    );
    crate::validate::message(&request.decision_message()).unwrap();
}

#[tokio::test]
async fn resource_release_targets_both_scopes_and_rolls_back_invalid_candidates() -> Result<()> {
    let (root, manager) = crate::test_support::manager().await;
    let path = root.path().to_owned();
    manager.store.run(move |db| {
            owners(db)?;
            db.execute("UPDATE workspaces SET path=?1 WHERE id='owner'", [path.to_str().unwrap()])?;
            for (id, scope, status) in [
                ("global", Scope::Global, DecisionStatus::Denied),
                ("repo", Scope::Repo("repo".into()), DecisionStatus::Approved),
            ] {
                let mut request = sample("owner", Target::Resource { scope, pool: "lock".into() });
                request.key = id.into();
                request.status = status;
                save(db, &request)?;
            }
            let Specification::Resource(spec) = resource(LockMode::Read) else { unreachable!() };
            db.execute("INSERT INTO resource_pools VALUES ('global','lock',?1)", [serde_json::to_string(&spec.definition)?])?;
            db.execute("INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at,mode)
                VALUES ('lease','owner','global','lock','default','lock',1,'read')", [])?;
            // Unrelated port history cannot block release, even without configured definitions.
            db.execute("INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number) VALUES ('port','owner','port/web','default','invalid',1,100)", [])?;
            db.execute("UPDATE access_requests SET record=json_set(record,'$.status','unknown') WHERE id='repo'", [])?;
            Ok(())
        }).await?;
    assert!(
        manager
            .release_resource("owner", "lock".into(), "default".into())
            .await
            .is_err()
    );
    manager.store.run(|db| {
            assert!(by_key(db, "global")?.is_some());
            assert_eq!(db.query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r.get::<_, i64>(0))?, 1);
            db.execute("UPDATE access_requests SET record=json_set(record,'$.status','approved') WHERE id='repo'", [])?;
            Ok(())
        }).await?;
    manager
        .release_resource("owner", "lock".into(), "default".into())
        .await?;
    manager
        .store
        .run(|db| {
            assert!(by_key(db, "global")?.is_none());
            assert!(!by_key(db, "repo")?.unwrap().active);
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM resource_leases", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert!(list(db, Some("owner")).is_err());
            Ok(())
        })
        .await
}

fn unrelated_history(db: &Connection, count: i64) -> Result<()> {
    db.execute(
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<?1)
            INSERT INTO access_requests(id,workspace_id,target_key,name,record,active,number)
            SELECT 'history-'||x, CASE WHEN x%2=0 THEN 'owner' ELSE 'other' END,
                CASE WHEN x%2=0 THEN 'port/other' ELSE 'resource/global/lock' END,
                'default', 'invalid', 0, 100000+x FROM n",
        [count],
    )?;
    Ok(())
}

#[tokio::test]
async fn reuse_validates_all_candidates_but_not_unrelated_history() -> Result<()> {
    let (_root, manager) = crate::test_support::manager().await;
    manager
        .store
        .run(|db| {
            owners(db)?;
            unrelated_history(db, 10_000)?;
            let tx = db.transaction()?;
            let target = Target::Resource {
                scope: Scope::Global,
                pool: "lock".into(),
            };
            let request = sample("owner", target.clone());
            let mut grant = check(&tx, request.clone())?.unwrap();
            grant.status = DecisionStatus::Approved;
            save(&tx, &grant)?;
            assert!(release(&tx, "owner", &target, "default")?);
            assert!(!by_key(&tx, &grant.key)?.unwrap().active);
            assert!(check(&tx, request.clone())?.is_none());
            let mut another = request.clone();
            another.name = "another".into();
            assert!(check(&tx, another.clone())?.is_none());
            another.specification = resource(LockMode::Write);
            assert!(check(&tx, another)?.is_some());

            // Neither a different workspace nor target can authorize this request.
            for (owner, target) in [
                ("other", target.clone()),
                ("owner", Target::Port("other".into())),
            ] {
                let mut unrelated = grant.clone();
                unrelated.key = format!("unrelated-{owner}");
                unrelated.workspace_id = owner.into();
                unrelated.target = target;
                save(&tx, &unrelated)?;
            }
            grant.status = DecisionStatus::Denied;
            grant.active = false;
            save(&tx, &grant)?;
            assert!(check(&tx, request.clone())?.is_some());
            assert!(release(&tx, "owner", &target, "default")?);
            grant.status = DecisionStatus::Approved;
            save(&tx, &grant)?;

            let mut broken = grant.clone();
            broken.key = "broken".into();
            // Put corruption after a reusable grant: a match must not short-circuit decoding.
            for (field, value) in [
                ("status", serde_json::json!("unknown")),
                ("lifetime", serde_json::json!("forever")),
                ("target", serde_json::json!("resource/lock")),
                ("specification", serde_json::json!({"member": "lock"})),
            ] {
                save(&tx, &broken)?;
                let mut record = serde_json::to_value(&broken)?;
                record[field] = value;
                tx.execute(
                    "UPDATE access_requests SET record=?1 WHERE id='broken'",
                    [record.to_string()],
                )?;
                let invalid = format!(
                    "approval request {} has an invalid record",
                    number(&tx, "broken")?
                );
                assert!(
                    check(&tx, request.clone())
                        .unwrap_err()
                        .to_string()
                        .contains(&invalid)
                );
                assert!(by_key(&tx, "broken").is_err());
            }
            assert!(list(&tx, None).is_err());
            assert!(list(&tx, Some("owner")).is_err());
            assert!(list(&tx, Some("missing"))?.is_empty());
            Ok(())
        })
        .await
}

fn plan(db: &Connection, sql: &str, params: impl Params) -> Result<String> {
    Ok(db
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
        .query_map(params, |row| row.get::<_, String>(3))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .join("; "))
}

fn query_work(db: &Connection, sql: &str, params: impl Params) -> Result<(usize, i32)> {
    let mut statement = db.prepare(sql)?;
    let records = statement
        .query_map(params, record_row)?
        .map(|row| decode_record(row?))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        statement.get_status(rusqlite::StatementStatus::FullscanStep),
        0
    );
    Ok((
        records.len(),
        statement.get_status(rusqlite::StatementStatus::VmStep),
    ))
}

#[tokio::test]
async fn bundled_planner_bounds_lookups_and_limits_candidate_decoding() -> Result<()> {
    let (_root, manager) = crate::test_support::manager().await;
    manager.store.run(|db| {
            owners(db)?;
            let target = Target::Resource { scope: Scope::Global, pool: "lock".into() };
            let mut request = sample("owner", target.clone());
            request.key = "wanted".into();
            save(db, &request)?;
            request.key = "released".into();
            request.active = false;
            request.status = DecisionStatus::Approved;
            save(db, &request)?;
            let id_work = query_work(db, BY_KEY, ["wanted"])?;
            let current_work = query_work(db, CURRENT, params!["owner", target, "default"])?;
            let candidate_work = query_work(db, CANDIDATES, params!["owner", target])?;
            assert_eq!((id_work.0, current_work.0, candidate_work.0), (1, 1, 2));
            unrelated_history(db, 10_000)?;
            assert_eq!(query_work(db, BY_KEY, ["wanted"])?, id_work);
            assert_eq!(query_work(db, CURRENT, params!["owner", target, "default"])?, current_work);
            assert_eq!(query_work(db, CANDIDATES, params!["owner", target])?, candidate_work);
            let id_plan = plan(db, BY_KEY, ["wanted"])?;
            let current_plan = plan(db, CURRENT, params!["owner", target, "default"])?;
            let candidate_plan = plan(db, CANDIDATES, params!["owner", target])?;
            assert!(id_plan.contains("SEARCH access_requests USING INDEX sqlite_autoindex_access_requests_1 (id=?)"), "{id_plan}");
            assert!(current_plan.contains("SEARCH access_requests USING INDEX access_request_name (workspace_id=? AND target_key=? AND name=?)"), "{current_plan}");
            assert!(candidate_plan.contains("SEARCH access_requests USING INDEX access_request_target (workspace_id=? AND target_key=?)"), "{candidate_plan}");
            db.execute_batch("SAVEPOINT without_index; DROP INDEX access_request_target;")?;
            let old_plan = plan(db, CANDIDATES, params!["owner", target])?;
            assert!(old_plan.contains("SCAN access_requests"), "{old_plan}");
            db.execute_batch("ROLLBACK TO without_index; RELEASE without_index;")?;
            eprintln!("SQLite {}: ID: {id_plan}; current: {current_plan}; candidates: {candidate_plan}; without target index: {old_plan}", rusqlite::version());
            eprintln!("Before/after 10,000 unrelated records, (decoded rows, VM steps): ID {id_work:?}, current {current_work:?}, candidates {candidate_work:?}; no full-scan steps");
            Ok(())
        }).await
}

#[test]
fn targets_keep_their_stored_spelling() -> Result<()> {
    let db = Connection::open_in_memory()?;
    for (target, text) in [
        (
            Target::Resource {
                scope: Scope::Global,
                pool: "lock".into(),
            },
            "resource/global/lock",
        ),
        (
            Target::Resource {
                scope: Scope::Repo("repo-id".into()),
                pool: "lock".into(),
            },
            "resource/repo/repo-id/lock",
        ),
        (Target::Port("web".into()), "port/web"),
        (Target::Simulator, "simulator"),
        (Target::Secret("TOKEN".into()), "secret/TOKEN"),
        (Target::Hold("review".into()), "hold/review"),
        (Target::Land, "land"),
    ] {
        assert_eq!(target.to_string(), text);
        assert_eq!(text.parse::<Target>()?, target);
        assert_eq!(serde_json::to_value(&target)?, text);
        assert_eq!(serde_json::from_value::<Target>(text.into())?, target);
        let stored: Target = db.query_row("SELECT ?1", [&target], |row| row.get(0))?;
        assert_eq!(stored, target);
    }
    for text in [
        "",
        "port/",
        "resource/global",
        "resource/global/",
        "resource/local/lock",
        "resource//lock",
        "simulators",
        "secret/",
        "hold/",
        "keep",
    ] {
        assert!(text.parse::<Target>().is_err(), "{text}");
    }
    Ok(())
}

#[test]
fn specifications_round_trip_recorded_json_and_reject_mixed_shapes() -> Result<()> {
    let recorded = [
        serde_json::json!({"definition": {"capacity": 1, "reason": null,
                "resources": {"lock": {"requires_approval": true, "approval_lifetime": "lease",
                    "kind": "semaphore", "capacity": 1, "reason": null}}},
                "member": "lock", "mode": "permit"}),
        serde_json::json!({"preferred": null, "env": "PORT_WEB", "on_conflict": "suggest",
                "range": [3000, 3100]}),
        serde_json::json!({"profile": {"requires_approval": true, "approval_lifetime": "workspace",
                "device": "iPhone", "runtime": "iOS"}, "clean": false}),
        serde_json::json!({"command": ["op", "read", "op://Agents/key"], "tools": ["agent-browser"]}),
        serde_json::json!({"branch": "feature", "head": "abc"}),
        serde_json::json!({"hold": "review"}),
    ];
    for json in recorded {
        let specification: Specification = serde_json::from_value(json.clone())?;
        assert_eq!(serde_json::to_value(&specification)?, json);
    }
    assert!(matches!(
        serde_json::from_value(serde_json::json!({"branch": "feature", "head": "abc"}))?,
        Specification::Land(_)
    ));
    assert!(matches!(
        serde_json::from_value(serde_json::json!({"preferred": 8080, "env": "PORT_WEB",
                "on_conflict": "auto", "range": [1, 2]}))?,
        Specification::Port(PortSpecification {
            preferred: Some(8080),
            ..
        })
    ));
    for json in [
        serde_json::json!({}),
        serde_json::json!({"member": "lock", "mode": "permit"}),
        serde_json::json!({"profile": {"device": "iPhone", "runtime": "iOS"}, "clean": false,
                "member": "lock"}),
        serde_json::json!({"preferred": null, "env": "PORT_WEB", "on_conflict": "maybe",
                "range": [3000, 3100]}),
        serde_json::json!({"command": ["op"], "tools": [], "head": "abc"}),
        serde_json::json!({"branch": "feature"}),
    ] {
        assert!(
            serde_json::from_value::<Specification>(json.clone()).is_err(),
            "{json}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn invalid_records_are_reported_instead_of_ignored() {
    let root = tempfile::tempdir().unwrap();
    let store = store::Store::open(root.path().join("state.db"))
        .await
        .unwrap();
    store.run(|db| {
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('owner','repo','worker','/work','worker','ready');")?;
            let request = ApprovalRequest::new("owner", Target::Simulator, "default",
                Specification::Simulator(SimulatorSpecification { clean: false, profile: Profile {
                    requires_approval: true, approval_lifetime: Lifetime::Lease,
                    device: "iPhone".into(), runtime: "iOS".into() } }), Lifetime::Lease, Some("test app"));
            let mut record = serde_json::to_value(&request)?;
            record["id"] = 1.into();
            let insert = |db: &Connection, key: &str, number: i64, record: &serde_json::Value| db.execute(
                "INSERT INTO access_requests(id,workspace_id,target_key,name,record,number) VALUES (?1,'owner','simulator',?1,?2,?3)",
                params![key, record.to_string(), number]);
            insert(db, "valid", 1, &record)?;
            assert_eq!(list(db, Some("owner"))?.len(), 1);
            let mut broken = record.clone();
            broken["specification"] = serde_json::json!({"member": "lock"});
            broken["id"] = 2.into();
            insert(db, "broken", 2, &broken)?;
            let error = list(db, Some("owner")).unwrap_err().to_string();
            assert!(error.contains("approval request 2 has an invalid record"), "{error}");
            db.execute("DELETE FROM access_requests WHERE id='broken'", [])?;
            broken["specification"] = record["specification"].clone();
            broken["target"] = "resource/lock".into();
            insert(db, "broken", 2, &broken)?;
            assert!(list(db, Some("owner")).is_err());
            Ok(())
        }).await.unwrap();
}

#[tokio::test]
async fn decisions_bind_settings_and_release_obeys_configured_lifetime() {
    let root = tempfile::tempdir().unwrap();
    let store = store::Store::open(root.path().join("state.db"))
        .await
        .unwrap();
    store.run(|db| {
            db.execute_batch("INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES ('owner','repo','worker','/work','worker','ready');")?;
            for lifetime in [Lifetime::Lease, Lifetime::Workspace] {
                assert_eq!(serde_json::to_value(lifetime)?, lifetime.to_string());
                let tx = db.transaction()?;
                let target = Target::Resource { scope: Scope::Global, pool: "lock".into() };
                let request = ApprovalRequest::new("owner", target, "default",
                    resource(LockMode::Read), lifetime, Some("read shared data"));
                let pending = check(&tx, request.clone())?.unwrap();
                assert_eq!(check(&tx, request.clone())?.unwrap().id, pending.id);
                assert_eq!(list(&tx, Some("owner"))?.len(), 1);
                let mut changed = request.clone();
                changed.specification = resource(LockMode::Write);
                assert!(check(&tx, changed).is_err());
                let mut decision = pending.clone();
                decision.status = DecisionStatus::Denied;
                tx.execute("UPDATE access_requests SET record=?2 WHERE id=?1", params![decision.key,serde_json::to_string(&decision)?])?;
                assert_eq!(check(&tx, request.clone())?.unwrap().status, DecisionStatus::Denied);
                decision.status = DecisionStatus::Approved;
                tx.execute("UPDATE access_requests SET record=?2 WHERE id=?1", params![decision.key,serde_json::to_string(&decision)?])?;
                assert!(check(&tx, request.clone())?.is_none());
                assert!(release(&tx, "owner", &request.target, "default")?);
                let result = check(&tx, request.clone())?;
                assert_eq!(result.is_some(), lifetime == Lifetime::Lease);
                if lifetime == Lifetime::Workspace {
                    let mut another = request.clone();
                    another.name = "another".into();
                    assert!(check(&tx, another.clone())?.is_none());
                    another.specification = resource(LockMode::Write);
                    assert!(check(&tx, another)?.is_some());
                }
                tx.rollback()?;
            }
            assert!(serde_json::from_str::<DecisionStatus>("\"unknown\"").is_err());
            assert!(serde_json::from_str::<Lifetime>("\"forever\"").is_err());
            Ok(())
        }).await.unwrap();
}
