use anyhow::Result;

use super::super::{
    activity::queue_update,
    tests::{CHILD_PR, GRANDCHILD_PR, register_watch},
};
use crate::{
    daemon::{
        test_support::{manager, repository},
        workspace::Manager,
    },
    forge::updates::{Update, UpdateKind},
    model::Workspace,
    state::AgentState,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::{link::Call, routing::Told},
    space_names::SpaceSelector,
};

/// A workspace below `away`, a space on another machine, and one split
/// from it, neither with an agent, on a machine joined to a hub it cannot
/// reach now.
async fn below_elsewhere(manager: &Manager, root: &std::path::Path) -> (Workspace, Workspace) {
    let checkout = repository(root, "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let child = (manager.create_workspace(&repo.id, "child".into(), None, None, None))
        .await
        .unwrap();
    let selector = SpaceSelector::id(&child.id);
    let grandchild =
        (manager.split_workspace(&selector, "grandchild".into(), None, <_>::default()))
            .await
            .unwrap();
    let id = child.id.clone();
    (manager.store)
        .write(move |tx| {
            crate::daemon::workspace::set_parent(tx, &id, Some(&SpaceId::from("away")))?;
            tx.execute(
                "INSERT INTO hub_membership(id, member_id, name, private_key, hub_key, url, joined_at)
                    VALUES (1, 'here', 'laptop', x'00', 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 'https://hub.example.com', 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (child, grandchild)
}

/// Queue a review of `url` on the cursor of the space `id`, which watches it.
async fn queue_review(manager: &Manager, id: &SpaceId, url: &str) {
    register_watch(manager, id, url).await;
    let update = Update {
        url: url.to_owned(),
        kind: UpdateKind::Comment,
        message: format!("approved {url}"),
        delivery: format!("review of {url}"),
        check: None,
        workspace: None,
    };
    let id = id.clone();
    (manager.store.run(move |db| queue_update(db, &id, update)))
        .await
        .unwrap();
}

/// What the outbox holds to tell, oldest first.
async fn outbox(manager: &Manager) -> Vec<Told> {
    let calls = (manager.store)
        .run(|db| {
            let mut statement = db.prepare("SELECT call FROM hub_outbox ORDER BY seq")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    (calls.iter())
        .map(|call| match serde_json::from_str(call).unwrap() {
            Call::Tell { told } => told,
            call => panic!("the outbox held {call:?}"),
        })
        .collect()
}

#[tokio::test]
async fn activity_of_spaces_below_a_coordinator_elsewhere_waits_in_the_outbox() -> Result<()> {
    let (root, manager) = manager().await;
    let (child, grandchild) = below_elsewhere(&manager, root.path()).await;
    queue_review(&manager, &child.id, &CHILD_PR).await;
    queue_review(&manager, &grandchild.id, &GRANDCHILD_PR).await;

    manager.pass_up_delegated_activity().await?;

    let told = outbox(&manager).await;
    let about = |told: &Told| told.about.as_ref().map(|about| about.space.clone());
    assert_eq!(
        told.iter().map(about).collect::<Vec<_>>(),
        [Some(child.id.clone()), Some(grandchild.id.clone())]
    );
    for (told, url) in told.iter().zip([&*CHILD_PR, &*GRANDCHILD_PR]) {
        assert_eq!(told.above, SpaceId::from("away"));
        assert!(
            told.message.contains(&format!("approved {url}")),
            "{}",
            told.message
        );
    }
    // Passed up, they leave the cursors, so the next sweep sends them no more.
    manager.pass_up_delegated_activity().await?;
    assert_eq!(outbox(&manager).await.len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_space_with_its_own_agent_keeps_its_activity() -> Result<()> {
    let (root, manager) = manager().await;
    let (child, grandchild) = below_elsewhere(&manager, root.path()).await;
    queue_review(&manager, &grandchild.id, &GRANDCHILD_PR).await;
    let selector = SpaceSelector::id(&grandchild.id);
    (manager.set_agent_state(&selector, AgentState::Working, None, None)).await?;
    queue_review(&manager, &child.id, &CHILD_PR).await;

    manager.pass_up_delegated_activity().await?;

    let told = outbox(&manager).await;
    let about = told
        .iter()
        .map(|told| told.about.as_ref().unwrap().space.clone());
    assert_eq!(about.collect::<Vec<_>>(), [child.id]);
    Ok(())
}
