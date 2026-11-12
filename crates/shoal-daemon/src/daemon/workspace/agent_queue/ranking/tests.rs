use std::collections::HashSet;

use rusqlite::params;

use super::*;
use crate::client::{resources::Scope, space_names::SpaceSelector};
use crate::{
    daemon::{
        test_support::manager,
        workspace::agent_queue::{
            Serving, last_seq,
            rank::{Edge, Node},
            tests::{add, create, limit_globally, register, request},
        },
    },
    model::{Blocker, Dependency, InheritedRank, QueueRank, QueueRequest, Tier},
    state::QueueState,
};

#[tokio::test]
async fn a_launch_in_a_child_ranks_with_its_ready_parent_ahead_of_earlier_launches() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "running").await;
    let parent = create(&manager, &repo, "parent").await;
    let child = create(&manager, &repo, "child").await;
    let ids = (parent.id.clone(), child.id.clone());
    manager
        .store
        .run(move |db| {
            let (parent, child) = &ids;
            db.execute(
                "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)",
                params![child, parent],
            )?;
            // A ready mark makes the parent's work finishing.
            db.execute(
                "INSERT INTO space_review(space_id,url,kind,head) VALUES (?1,'',NULL,'abc')",
                [parent],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    add(&manager, &repo, "earlier").await;
    manager
        .queue_add(QueueRequest {
            workspace: Some(SpaceSelector::id(&child.id)),
            ..request(&repo, "child", false)
        })
        .await
        .unwrap();

    let waiting: Vec<_> = (manager.queue_entries().await.unwrap().into_iter())
        .filter(|entry| entry.state == QueueState::Queued)
        .map(|entry| (entry.name.to_string(), entry.position, entry.rank))
        .collect();
    assert_eq!(
        waiting,
        [
            (
                "child".into(),
                Some(1),
                Some(QueueRank {
                    tier: Tier::Finishing,
                    inherited: Some(InheritedRank {
                        from: parent.number.to_string(),
                        dependency: Dependency::Parent,
                    }),
                })
            ),
            (
                "earlier".into(),
                Some(2),
                Some(QueueRank {
                    tier: Tier::New,
                    inherited: None,
                })
            ),
        ]
    );
}

#[tokio::test]
async fn waits_come_from_blockers_questions_stacks_and_leases() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    add(&manager, &repo, "running").await;
    let base = create(&manager, &repo, "base").await;
    let stacked = create(&manager, &repo, "stacked").await;
    let asking = create(&manager, &repo, "asking").await;
    let holder = create(&manager, &repo, "holder").await;
    let issue = "https://forge.example/team/repo/issues/7";
    let ids = [&base, &stacked, &asking, &holder].map(|workspace| workspace.id.clone());
    manager
        .store
        .run(move |db| {
            let [base, stacked, asking, holder] = &ids;
            db.execute(
                "UPDATE workspaces SET base_workspace_id=?1 WHERE id=?2",
                params![base, stacked],
            )?;
            db.execute(
                "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)",
                params![asking, stacked],
            )?;
            db.execute(
                "INSERT INTO questions(space_number,number,workspace_id,created_at,question,
                        recommendation,choices)
                    VALUES (9,1,?1,1,'Which?','This','[]')",
                [asking],
            )?;
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,?2)",
                params![holder, issue],
            )?;
            db.execute(
                "INSERT INTO resource_pools VALUES (?1,'devices','{}')",
                params![Scope::Global],
            )?;
            db.execute(
                "INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,created_at,mode)
                    VALUES ('lease',?1,?2,'devices','default','devices',1,'write')",
                params![holder, Scope::Global],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let blocked = manager
        .queue_add(QueueRequest {
            blockers: vec![Blocker::Issue(issue.into())],
            ..request(&repo, "blocked", false)
        })
        .await
        .unwrap()
        .entry
        .unwrap();
    let leases = vec![(
        Target::Resource {
            scope: Scope::Global,
            pool: "devices".into(),
        },
        base.id.clone(),
    )];
    let edges: HashSet<Edge> = manager
        .store
        .read(move |tx| edges::waits(tx, &leases))
        .await
        .unwrap()
        .into_iter()
        .collect();

    let space = |workspace: &crate::model::Workspace| Node::Space(workspace.id.clone());
    let edge = |waiting, on, dependency| Edge {
        waiting,
        on,
        dependency,
    };
    let expected = HashSet::from([
        edge(space(&stacked), space(&base), Dependency::Stacked),
        edge(space(&stacked), space(&asking), Dependency::Parent),
        edge(space(&asking), space(&stacked), Dependency::Question),
        edge(Node::Entry(blocked.id), space(&holder), Dependency::Blocked),
        edge(space(&base), space(&holder), Dependency::Lease),
    ]);
    assert_eq!(edges, expected);
}

#[tokio::test]
async fn a_space_with_a_pr_it_opened_finishes_until_the_pr_merges() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "topic").await;
    let pr = "https://forge.example/team/repo/pulls/3";
    let tier = |state: Option<&'static str>| {
        let id = workspace.id.clone();
        manager.store.read(move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO space_items(space_id,relation,kind,url) VALUES (?1,'opened','pr',?2)",
                params![id, pr],
            )?;
            tx.execute("DELETE FROM pr_activity WHERE space_id=?1", [&id])?;
            if let Some(state) = state {
                tx.execute(
                    "INSERT INTO pr_activity(space_id,url,record) VALUES (?1,?2,json_object('state',?3))",
                    params![id, pr, state],
                )?;
            }
            Ok(spaces::ranked(tx)?[&id].tier)
        })
    };
    // No watch has read the PR yet, so it counts as open.
    assert_eq!(tier(None).await.unwrap(), Tier::Finishing);
    assert_eq!(tier(Some("open")).await.unwrap(), Tier::Finishing);
    assert_eq!(tier(Some("merged")).await.unwrap(), Tier::New);
    // The poll saw it merge while the watch's cursor still says open.
    let id = workspace.id.clone();
    let tier = manager.store.read(move |tx| {
        tx.execute(
            "INSERT INTO space_item_activity(space_id,url,record) VALUES (?1,?2,json_object('state','merged'))",
            params![id, pr],
        )?;
        tx.execute("UPDATE pr_activity SET record=json_object('state','open') WHERE space_id=?1", [&id])?;
        Ok(spaces::ranked(tx)?[&id].tier)
    });
    assert_eq!(tier.await.unwrap(), Tier::New);
}

#[tokio::test]
async fn a_review_arrives_after_the_entries_queued_before_it_in_the_same_second() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let id = repo.id.clone();
    let queued_after = manager
        .store
        .run(move |db| {
            let insert = |entry: &str| {
                db.execute(
                    "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,state,created_at)
                     VALUES (?1,'launch',?2,?1,'claude','{}','{}','queued',10)",
                    params![entry, id],
                )
            };
            insert("early")?;
            let queued_after = last_seq(db)?;
            insert("late")?;
            Ok(queued_after)
        })
        .await
        .unwrap();
    let review = WaitingOneShot {
        id: String::from("oneshot:0").into(),
        repository: repo.id.clone(),
        serving: Some(Serving {
            space: String::from("reviewed").into(),
            queued_after,
        }),
        requested_at: UnixTime::from_seconds(10),
        after_stop: false,
    };
    let entries = manager
        .store
        .read(move |tx| entries::waiting(tx, &[review]))
        .await
        .unwrap();
    let arrived: Vec<_> = (entries.iter())
        .map(|entry| (entry.id.to_string(), entry.arrival))
        .collect();
    assert_eq!(
        arrived,
        [
            ("early".into(), 0),
            ("oneshot:0".into(), 1),
            ("late".into(), 2)
        ]
    );
}

#[tokio::test]
async fn the_last_sequence_number_holds_after_the_newest_entry_leaves() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let id = repo.id.clone();
    let (before, after) = manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,state,created_at)
                 VALUES ('gone','launch',?1,'gone','claude','{}','{}','queued',10)",
                [&id],
            )?;
            let before = last_seq(db)?;
            db.execute("DELETE FROM agent_queue WHERE id='gone'", [])?;
            Ok((before, last_seq(db)?))
        })
        .await
        .unwrap();
    assert!(before > 0);
    assert_eq!(after, before);
}
