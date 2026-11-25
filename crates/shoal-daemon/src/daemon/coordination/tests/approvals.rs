#[allow(unused_imports)]
use crate::daemon::manager::Manager;
use crate::daemon::resources::{Definition, LockMode, Scope};
use crate::daemon::scope::Access;
use crate::daemon::{
    item_numbers::{self, ItemNumber},
    notifications::AlertKind,
    scope::Caller,
};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_coordination::approvals::store::{by_key, list};
use shoal_coordination::approvals::text::RequestText;
use shoal_coordination::approvals::*;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::config::repo::Lifetime;
use shoal_protocol::space_names::SpaceSelector;

fn resource(mode: LockMode) -> Specification {
    let definition = Definition {
        capacity: 1,
        reason: None,
        resources: std::collections::BTreeMap::from([(
            "lock".to_owned(),
            crate::daemon::resources::ResourceConfig::default(),
        )]),
    };
    Specification::Resource(shoal_protocol::approvals::ResourceSpecification {
        repository: None,
        definition,
        member: "lock".into(),
        mode,
    })
}

fn owners(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used,workspaces_dir) VALUES ('repo','/repo','/repo',1,'/shoal/repo');
            BEGIN;
            INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                VALUES ('owner','/work','worker','ready','abc','/admin'),
                ('other','/other','other','ready','abc','/admin');
            INSERT INTO spaces(id,kind,repository_id,name)
                VALUES ('owner','workspace','repo','worker'),
                ('other','workspace','repo','other');
            COMMIT;",
    )?;
    Ok(())
}

/// Store `request` under its key, keeping the number a stored request
/// with that key holds or taking a free one.
fn save(db: &Connection, request: &ApprovalRequest) -> Result<()> {
    let number = match db
        .query_row(
            "SELECT space_number,number FROM access_requests WHERE id=?1",
            [&request.key],
            |r| {
                Ok(ItemNumber {
                    space: r.get(0)?,
                    item: r.get(1)?,
                })
            },
        )
        .optional()?
    {
        Some(number) => number,
        None => item_numbers::next(db, &request.space_id)?,
    };
    db.execute(
        "INSERT OR REPLACE INTO access_requests(id,space_id,target_key,name,record,active,
                space_number,number)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            request.key,
            request.space_id,
            request.target,
            request.name,
            serde_json::to_string(request)?,
            request.active,
            number.space,
            number.item
        ],
    )?;
    Ok(())
}

/// Item `item` of the space numbered `space`.
fn item(space: u32, item: u32) -> ItemNumber {
    ItemNumber {
        space: space.into(),
        item,
    }
}

fn sample(owner: &SpaceId, target: Target) -> ApprovalRequest {
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
async fn decisions_decode_only_the_selected_id_and_remain_idempotent() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    manager.store.run(|db| {
            owners(db)?;
            for key in ["approve", "deny"] {
                let mut request = sample(&SpaceId::from("owner"), Target::Simulator);
                request.key = key.into();
                request.name = key.into();
                save(db, &request)?;
            }
            db.execute("INSERT INTO access_requests(id,space_id,target_key,name,record,active,space_number,number) VALUES ('broken','other','simulator','default','invalid',0,2,3)", [])?;
            Ok(())
        }).await?;
    for (id, approve, status) in [
        (item(1, 1), true, DecisionStatus::Approved),
        (item(1, 2), false, DecisionStatus::Denied),
    ] {
        let decision = manager
            .coordination
            .decide_approval(&*manager, id, approve, None, Default::default(), None)
            .await?;
        assert_eq!(decision.status, status);
        let messages = manager
            .agents
            .agent_messages(&*manager, &manager.named("worker").await)
            .await?;
        let ids: Vec<_> = messages.iter().map(|m| m.id).collect();
        assert_eq!(
            messages.into_iter().map(|m| m.message).collect::<Vec<_>>(),
            [decision.decision_message()]
        );
        manager
            .agents
            .mark_agent_messages_delivered(&*manager, &manager.named("worker").await, ids)
            .await?;
        let retry = manager
            .coordination
            .decide_approval(&*manager, id, approve, None, Default::default(), None)
            .await?;
        assert_eq!(
            serde_json::to_value(retry)?,
            serde_json::to_value(&decision)?
        );
        // Repeating a decision does not tell the agent again.
        assert!(
            manager
                .agents
                .agent_messages(&*manager, &manager.named("worker").await)
                .await?
                .is_empty()
        );
        assert!(
            manager
                .coordination
                .decide_approval(&*manager, id, !approve, None, Default::default(), None)
                .await
                .is_err()
        );
    }
    // Each decision reaches the space's log once, though repeated.
    let decided: Vec<_> = (manager.space_events(None, 100).await?.into_iter())
        .filter_map(|item| match item {
            crate::daemon::events::EventItem::Event(event) => event.details.approval,
            _ => None,
        })
        .map(|approval| (approval.number, approval.status))
        .collect();
    assert_eq!(
        decided,
        [
            (item(1, 1), DecisionStatus::Approved),
            (item(1, 2), DecisionStatus::Denied)
        ]
    );
    // A decision chosen from a shown request does not reach another
    // request holding its number.
    let shown = (manager
        .coordination
        .approval_requests(&*manager, Some(&manager.named("worker").await)))
    .await?;
    manager
        .coordination
        .decide_approval(
            &*manager,
            item(1, 1),
            true,
            Some(shown[1].clone()),
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    // Displayed snapshots travel through JSON to the CLI and back.
    let snapshot: ApprovalRequest = serde_json::from_value(serde_json::to_value(&shown[0])?)?;
    let mut replaced = snapshot.clone();
    replaced.key = "a different request with the same visible details".into();
    manager
        .coordination
        .decide_approval(
            &*manager,
            item(1, 1),
            true,
            Some(replaced),
            Default::default(),
            None,
        )
        .await
        .unwrap_err();
    manager
        .coordination
        .decide_approval(
            &*manager,
            item(1, 1),
            true,
            Some(snapshot),
            Default::default(),
            None,
        )
        .await?;
    for id in [item(1, 4), item(2, 3)] {
        assert!(
            manager
                .coordination
                .decide_approval(&*manager, id, true, None, Default::default(), None)
                .await
                .is_err()
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
            .coordination
            .decide_approval(&*manager, item(1, 1), true, None, Default::default(), None)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn withdrawn_requests_keep_their_numbers_and_take_back_their_notifications() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
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
            .coordination
            .request_approval(sample(&workspace.id, Target::Port("web".into())))
            .await
            .unwrap()
            .unwrap();
        manager
            .coordination
            .notify_approval(&*manager, &request)
            .await;
        request
    };
    let unseen = request().await;
    assert!(
        manager
            .coordination
            .withdraw_approval(&*manager, &SpaceSelector::id(&other.id), unseen.number)
            .await
            .is_err()
    );

    let withdrawn = manager
        .coordination
        .withdraw_approval(&*manager, &SpaceSelector::id(&workspace.id), unseen.number)
        .await
        .unwrap();
    assert_eq!(withdrawn.status, DecisionStatus::Withdrawn);
    assert!(!withdrawn.active);
    assert!(
        manager
            .notifications
            .list(true, 50)
            .await
            .unwrap()
            .is_empty()
    );
    let default = proposals::ProposalDecision::default;
    let decided =
        manager
            .coordination
            .decide_approval(&*manager, unseen.number, true, None, default(), None);
    assert!(decided.await.is_err());
    let selected = SpaceSelector::id(&workspace.id);
    let withdrawn = manager
        .coordination
        .withdraw_approval(&*manager, &selected, unseen.number);
    assert!(withdrawn.await.is_err());

    // Requesting again asks anew under another number; once the user saw
    // that one, withdrawing it tells them.
    let shown = request().await;
    assert_ne!(shown.number, unseen.number);
    let notifications = manager.notifications.list(true, 50).await.unwrap();
    manager
        .notifications
        .mark_read(notifications.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    manager
        .coordination
        .withdraw_approval(&*manager, &SpaceSelector::id(&workspace.id), shown.number)
        .await
        .unwrap();
    let unread = manager.notifications.list(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].kind, AlertKind::ApprovalWithdrawn);

    // Workspace processes withdraw only their own requests.
    let caller = crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    manager
        .agents
        .executions()
        .issue_scope("scope".into(), caller)
        .await;
    for (target, allowed) in [(&workspace.id, true), (&other.id, false)] {
        let mut method = crate::protocol::ApprovalMethod::WithdrawApproval {
            space: SpaceSelector::from(target),
            number: shown.number,
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method)
                .await
                .is_ok();
        assert_eq!(authorized, allowed);
    }
}

#[tokio::test]
async fn a_chat_withdraws_only_its_own_requests() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    let request = manager
        .coordination
        .request_approval(sample(&workspace.id, Target::Port("web".into())))
        .await
        .unwrap()
        .unwrap();
    assert!(
        manager
            .coordination
            .withdraw_approval(&*manager, &SpaceSelector::id(&chat.id), request.number)
            .await
            .is_err()
    );
    let pending = manager
        .coordination
        .show_approval(request.number)
        .await
        .unwrap();
    assert_eq!(pending.status, DecisionStatus::Pending);
}

#[tokio::test]
async fn a_withdrawn_request_is_never_announced_afterwards() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    let request = manager
        .coordination
        .request_approval(sample(&workspace.id, Target::Port("web".into())))
        .await
        .unwrap()
        .unwrap();
    manager
        .coordination
        .withdraw_approval(&*manager, &SpaceSelector::id(&workspace.id), request.number)
        .await
        .unwrap();
    // The allocation announces the request it recorded only now.
    manager
        .coordination
        .notify_approval(&*manager, &request)
        .await;
    let unread = manager.notifications.list(true, 50).await.unwrap();
    assert!(
        unread
            .iter()
            .all(|n| n.kind != AlertKind::ApprovalRequested),
        "{unread:?}"
    );
}

#[tokio::test]
async fn resource_release_targets_both_scopes_and_rolls_back_invalid_candidates() -> Result<()> {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let path = root.path().to_owned();
    manager.store.run(move |db| {
            owners(db)?;
            db.execute("UPDATE workspaces SET path=?1 WHERE id='owner'", [path.to_str().unwrap()])?;
            for (id, scope, status) in [
                ("global", Scope::Global, DecisionStatus::Denied),
                ("repo", Scope::Repo("repo".into()), DecisionStatus::Approved),
            ] {
                let mut request = sample(&SpaceId::from("owner"), Target::Resource { scope, pool: "lock".into() });
                request.key = id.into();
                request.status = status;
                save(db, &request)?;
            }
            let Specification::Resource(spec) = resource(LockMode::Read) else { unreachable!() };
            db.execute("INSERT INTO resource_pools VALUES ('global','lock',?1)", [serde_json::to_string(&spec.definition)?])?;
            db.execute("INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at,mode)
                VALUES ('lease','owner','global','lock','default','lock',1,'read')", [])?;
            // Unrelated port history cannot block release, even without configured definitions.
            db.execute("INSERT INTO access_requests(id,space_id,target_key,name,record,active,number) VALUES ('port','owner','port/web','default','invalid',1,100)", [])?;
            db.execute("UPDATE access_requests SET record=json_set(record,'$.status','unknown') WHERE id='repo'", [])?;
            Ok(())
        }).await?;
    assert!(
        manager
            .leases
            .release_resource(&*manager, &"owner".into(), "lock".into(), "default".into())
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
        .leases
        .release_resource(&*manager, &"owner".into(), "lock".into(), "default".into())
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
            assert!(list(db, Some(&SpaceId::from("owner"))).is_err());
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_parent_approves_a_merge_with_its_own_merge_permission_at_its_commit() {
    use crate::daemon::test_support::{chat, commit, manager, repository, working_below};
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let config = "[modes.chat.permissions]\nmerge = 'direct'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let top = chat(&manager, "top", "chat", Some(&repo), None).await;
    let worker = working_below(&manager, &repo, "worker", &top.id, "worker").await;
    let child = working_below(&manager, &repo, "child", &worker.id, "worker").await;
    // Each request goes to the worker, which asks before it merges, so it
    // may only deny it or pass it up to the chat.
    let request = || async {
        let requested = (manager
            .coordination
            .request_merge_approval(merge_request(&child)))
        .await
        .unwrap()
        .unwrap();
        manager
            .coordination
            .notify_approval(&*manager, &requested)
            .await;
        let deciding = Caller::of_space(worker.id.clone());
        let approved = manager.coordination.decide_routed_approval(
            &*manager,
            &deciding,
            requested.number,
            true,
            None,
            Default::default(),
        );
        assert!(approved.await.is_err());
        let selected = SpaceSelector::id(&worker.id);
        let passed = manager.coordination.escalate_approval(
            &*manager,
            None,
            &selected,
            requested.number,
            None,
        );
        assert_eq!(passed.await.unwrap().decider, Some(top.id.clone()));
        requested
    };

    // The chat merges directly, so it approves.
    let requested = request().await;
    let approved = manager
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(top.id.clone()),
            requested.number,
            true,
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(approved.status, DecisionStatus::Approved);

    // A branch that moved since the request makes it stale for the chat, as
    // for the user.
    std::fs::write(child.path.join("tracked"), "changed\n").unwrap();
    commit(&child.path, "tracked");
    let requested = request().await;
    std::fs::write(child.path.join("tracked"), "changed again\n").unwrap();
    commit(&child.path, "tracked");
    let stale = manager
        .coordination
        .decide_routed_approval(
            &*manager,
            &Caller::of_space(top.id.clone()),
            requested.number,
            true,
            None,
            Default::default(),
        )
        .await;
    assert!(stale.is_err());
    let retired = manager
        .coordination
        .approval_by_key(&requested.key)
        .await
        .unwrap();
    assert_eq!(retired.unwrap().status, DecisionStatus::Outdated);
}

/// The request to merge `workspace`'s branch at its current commit.
fn merge_request(workspace: &crate::model::Workspace) -> ApprovalRequest {
    let head = crate::test_support::git(&workspace.path, &["rev-parse", "HEAD"]);
    let merge = MergeSpecification {
        branch: workspace.branch().cloned().unwrap(),
        head: head.trim().into(),
        method: crate::forge::items::action::MergeMethod::Merge,
        message: None,
        push: None,
        unpushed: None,
        changed_on_both_sides: Vec::new(),
    };
    let specification = Specification::Merge(merge);
    ApprovalRequest::new(
        &workspace.id,
        Target::Merge,
        MERGE,
        specification,
        Lifetime::Lease,
        Some("Merges the branch"),
    )
}
