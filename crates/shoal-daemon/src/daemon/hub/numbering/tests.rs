use anyhow::Context as _;
use shoal_protocol::{questions::Ask, space_names::SpaceSelector};

use super::*;
use crate::{
    config::permissions::PermissionsConfig,
    daemon::{item_numbers::ItemNumber, spaces, test_support, workspace_files::Attacher},
    model::SpaceKind,
};

/// Record a chat `id` as a new space is, and its number.
fn record(tx: &Transaction<'_>, id: &str) -> Result<SpaceNumber> {
    let (id, name) = (SpaceId::from(id), SpaceName::from(id));
    let new = spaces::records::NewSpace {
        id: &id,
        kind: SpaceKind::Chat,
        repository_id: None,
        name: &name,
        number: SpaceNumber::default(),
        mode: None,
        narrowing: &PermissionsConfig::default(),
        created_at: crate::time::UnixTime::now(),
    };
    spaces::records::record(tx, &new)
}

fn spares(numbers: &[u32]) -> Numbering {
    Numbering {
        spares: numbers.iter().copied().map(SpaceNumber::new).collect(),
        renumbered: Vec::new(),
    }
}

#[tokio::test]
async fn a_machine_of_a_hub_numbers_new_spaces_from_its_spares() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let numbers = (manager.store)
        .write(|tx| {
            let alone = record(tx, "alone")?;
            keep_spares(tx, &spares(&[1, 4]))?;
            // A spare a space here holds is passed over, and once none is
            // left a space takes the number above the highest here.
            let numbers = [alone, record(tx, "spare")?, record(tx, "above")?];
            Ok(numbers.map(SpaceNumber::get))
        })
        .await?;
    assert_eq!(numbers, [1, 4, 5]);
    Ok(())
}

#[tokio::test]
async fn a_machine_that_leaves_its_hub_numbers_on_its_own() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let numbered = (manager.store)
        .write(|tx| {
            tx.execute_batch(
                "INSERT INTO hub_membership(id, member_id, name, private_key, hub_key, url, joined_at)
                    VALUES (1, 'm', 'desk', x'00', 'k', 'https://hub.example.com', 0);",
            )?;
            keep_spares(tx, &spares(&[7]))?;
            forget_unless_joined(tx)?;
            let joined = next(tx, &SpaceId::from("new"))?;
            tx.execute("UPDATE hub_membership SET revoked=1", [])?;
            forget_unless_joined(tx)?;
            Ok((joined, next(tx, &SpaceId::from("new"))?))
        })
        .await?;
    assert_eq!(numbered, (Some(SpaceNumber::new(7)), None));
    Ok(())
}

#[tokio::test]
async fn a_renumbered_space_takes_its_questions_and_their_waits_along() -> Result<()> {
    let (root, manager) = test_support::manager().await;
    let path = test_support::repository(root.path(), "repo");
    let repo = (manager.register_repository(path.to_str().unwrap().into(), None, None)).await?;
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let (asker, other) = (create("asker").await?, create("other").await?);
    let selector = SpaceSelector::id(&asker.id);
    let ask = Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    let asked = manager.ask_question(&selector, ask.clone(), Vec::new(), &Attacher::User);
    let item = asked.await?.number.item;
    let waiter = tokio::spawn({
        let (manager, selector) = (manager.clone(), selector.clone());
        async move { manager.await_answer(&selector, item, &ask).await }
    });
    let renumbering = |space: &SpaceId| Numbering {
        spares: Vec::new(),
        renumbered: vec![Renumbering {
            space: space.clone(),
            number: SpaceNumber::new(4),
        }],
    };
    manager.take_numbering(renumbering(&asker.id)).await?;
    // A number a space here holds is not taken.
    manager.take_numbering(renumbering(&other.id)).await?;
    let mut numbers = Vec::new();
    for id in [&asker.id, &other.id] {
        numbers.push(manager.space(&SpaceSelector::id(id)).await?.number.get());
    }
    assert_eq!(numbers, [4, 2]);
    let renumbered = ItemNumber {
        space: SpaceNumber::new(4),
        item,
    };
    manager
        .answer_question(renumbered, "Postgres".into(), None)
        .await?;
    assert_eq!(waiter.await??.status.answer(), Some("Postgres"));
    let notifications = manager.notifications(false, 50).await?;
    let notice = (notifications.iter())
        .find(|notification| notification.kind == NotificationKind::SpaceRenumbered)
        .context("no notice of the renumbering")?;
    assert!(
        notice.message.contains("space 4") && notice.message.contains("not 1"),
        "{}",
        notice.message
    );
    Ok(())
}

#[tokio::test]
async fn an_imported_space_keeps_the_number_the_hub_kept_for_it() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    for (id, number) in [("moved", 5), ("taken", 1)] {
        let number = SpaceNumber::new(number);
        manager
            .keep_imported_number(&SpaceId::from(id), number)
            .await?;
    }
    let numbers = (manager.store)
        .write(|tx| {
            let numbers = [
                record(tx, "here")?,
                record(tx, "moved")?,
                record(tx, "taken")?,
            ];
            Ok(numbers.map(SpaceNumber::get))
        })
        .await?;
    // A number a space here holds is not kept.
    assert_eq!(numbers, [1, 5, 2]);
    Ok(())
}
