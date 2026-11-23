use anyhow::Result;
use serde_json::json;

use super::super::test_support::register_watch;
use super::super::tests::{CHILD_PR, GRANDCHILD_PR};
use crate::{
    daemon::{
        manager::Manager,
        test_support::{configure, manager, repository},
    },
    forge::updates::{Update, UpdateKind},
    model::Workspace,
    state::AgentState,
};
use shoal_ids::space_ids::SpaceId;
use shoal_links::wait::changes::queue_update;
use shoal_links::wait::elsewhere::EVENT_LIMIT;
use shoal_protocol::{
    hub::{link::Call, routing::Told},
    space_names::SpaceSelector,
};

/// A public key, as the store keeps one.
const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// A workspace below `away`, a space with an agent on the offline machine
/// `desk`, and one split from it, neither with an agent, on the hub's own
/// machine.
async fn below_elsewhere(manager: &Manager, root: &std::path::Path) -> (Workspace, Workspace) {
    configure(manager, "[machine.hub]\nenabled = true\n").await;
    let checkout = repository(root, "repo");
    let repo = (manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let child = (manager.create_workspace(&repo.id, "child".into(), None, None, None))
        .await
        .unwrap();
    let selector = SpaceSelector::id(&child.id);
    let grandchild = (manager.spaces.create_child(
        manager,
        &selector,
        "grandchild".into(),
        None,
        <_>::default(),
    ))
    .await
    .unwrap();
    let away = json!({
        "id": "away", "number": 90, "name": "away", "kind": "chat",
        "status": "working", "agent": true,
    });
    let id = child.id.clone();
    (manager.store)
        .write(move |tx| {
            crate::daemon::tree::set_parent(tx, &id, Some(&SpaceId::from("away")))?;
            tx.execute(
                "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
                    VALUES ('desk', 'machine', 'desk', ?1, 0)",
                [KEY],
            )?;
            tx.execute(
                "INSERT INTO hub_spaces(space_id, member_id, summary, reported_at)
                    VALUES ('away', 'desk', ?1, 0)",
                [away.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (child, grandchild)
}

/// Queue an update of `url` whose message is `message` on the cursor of
/// the space `id`, which watches it.
async fn queue_comment(manager: &Manager, id: &SpaceId, url: &str, message: String) {
    register_watch(manager, id, url).await;
    let update = Update {
        url: url.to_owned(),
        kind: UpdateKind::Comment,
        delivery: format!("{url} {}", message.len()),
        message,
        check: None,
        workspace: None,
    };
    let id = id.clone();
    (manager.store.run(move |db| queue_update(db, &id, update)))
        .await
        .unwrap();
}

/// What the hub holds for the offline machine `desk` to tell, oldest first.
async fn held(manager: &Manager) -> Vec<Told> {
    let calls = (manager.store)
        .run(|db| {
            let mut statement =
                db.prepare("SELECT call FROM hub_queue WHERE member_id='desk' ORDER BY seq")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    (calls.iter())
        .map(|call| match serde_json::from_str(call).unwrap() {
            Call::Tell { told } => told,
            call => panic!("the hub held {call:?}"),
        })
        .collect()
}

/// The space each event in `told` is about.
fn about(told: &[Told]) -> Vec<SpaceId> {
    let about = told
        .iter()
        .map(|told| told.about.as_ref().unwrap().space.clone());
    about.collect()
}

#[tokio::test]
async fn the_hub_holds_the_activity_of_spaces_below_an_offline_coordinator() -> Result<()> {
    let (root, manager) = manager().await;
    let (child, grandchild) = below_elsewhere(&manager, root.path()).await;
    queue_comment(&manager, &child.id, &CHILD_PR, "looks good".into()).await;
    queue_comment(&manager, &grandchild.id, &GRANDCHILD_PR, "one nit".into()).await;

    manager.links.pass_up_delegated_activity(&*manager).await?;

    let told = held(&manager).await;
    assert_eq!(about(&told), [child.id.clone(), grandchild.id.clone()]);
    for (told, url) in told.iter().zip([&*CHILD_PR, &*GRANDCHILD_PR]) {
        assert_eq!(told.above, SpaceId::from("away"));
        assert!(text(told).contains(url.as_str()), "{}", text(told));
    }
    // Taken by the hub, they leave the cursors, so the next sweep sends
    // them no more.
    manager.links.pass_up_delegated_activity(&*manager).await?;
    assert_eq!(held(&manager).await.len(), 2);
    Ok(())
}

#[tokio::test]
async fn activity_too_large_for_one_event_goes_in_several() -> Result<()> {
    let (root, manager) = manager().await;
    let (child, _) = below_elsewhere(&manager, root.path()).await;
    let long = "a".repeat(2 * EVENT_LIMIT);
    queue_comment(&manager, &child.id, &CHILD_PR, long).await;
    queue_comment(&manager, &child.id, &CHILD_PR, "short".into()).await;

    manager.links.pass_up_delegated_activity(&*manager).await?;
    let first = held(&manager).await;
    assert_eq!(first.len(), 1);
    assert!(
        text(&first[0]).len() <= EVENT_LIMIT,
        "{}",
        text(&first[0]).len()
    );

    manager.links.pass_up_delegated_activity(&*manager).await?;
    let told = held(&manager).await;
    assert_eq!(told.len(), 2);
    assert!(text(&told[1]).ends_with("short"), "{}", text(&told[1]));
    Ok(())
}

#[tokio::test]
async fn a_space_with_its_own_agent_keeps_its_activity() -> Result<()> {
    let (root, manager) = manager().await;
    let (child, grandchild) = below_elsewhere(&manager, root.path()).await;
    queue_comment(&manager, &grandchild.id, &GRANDCHILD_PR, "mine".into()).await;
    let selector = SpaceSelector::id(&grandchild.id);
    (manager
        .agents
        .set_agent_state(&*manager, &selector, AgentState::Working, None, None))
    .await?;
    queue_comment(&manager, &child.id, &CHILD_PR, "theirs".into()).await;

    manager.links.pass_up_delegated_activity(&*manager).await?;

    assert_eq!(about(&held(&manager).await), [child.id]);
    Ok(())
}

#[tokio::test]
async fn activity_stays_on_the_cursor_while_the_hub_cannot_be_reached() -> Result<()> {
    let (root, manager) = manager().await;
    let (child, _) = below_elsewhere(&manager, root.path()).await;
    // A member whose link is down, rather than the hub.
    configure(&manager, "").await;
    (manager.store)
        .write(|tx| {
            tx.execute(
                "INSERT INTO hub_membership(id, member_id, name, private_key, hub_key, url, joined_at)
                    VALUES (1, 'here', 'laptop', x'00', ?1, 'https://hub.example.com', 0)",
                [KEY],
            )?;
            Ok(())
        })
        .await?;
    queue_comment(&manager, &child.id, &CHILD_PR, "while away".into()).await;

    manager.links.pass_up_delegated_activity(&*manager).await?;

    let outbox = (manager.store)
        .run(|db| {
            Ok(db.query_row("SELECT COUNT(*) FROM hub_outbox", [], |row| {
                row.get::<_, i64>(0)
            })?)
        })
        .await?;
    assert_eq!(outbox, 0_i64);
    let pending = (manager.store)
        .run(|db| {
            Ok(
                db.query_row("SELECT COUNT(*) FROM pending_updates", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await?;
    assert_eq!(pending, 1_i64);
    Ok(())
}

/// The text `told` tells.
fn text(told: &shoal_protocol::hub::routing::Told) -> String {
    told.record.texts().1
}
