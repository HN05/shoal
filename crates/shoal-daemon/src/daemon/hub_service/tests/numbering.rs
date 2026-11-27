use anyhow::{Context as _, Result};
use rusqlite::{Transaction, params};
use shoal_hub::daemon::Records as _;
use shoal_hub::numbering::{forget_unless_joined, keep_spares, next};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    alerts::AlertKind,
    hub::numbering::{Numbering, Renumbering},
    space_names::{SpaceName, SpaceSelector},
};
use shoal_store::test_support::seeds;

use crate::{
    config::permissions::PermissionsConfig,
    daemon::{item_numbers::ItemNumber, spaces, test_support},
    model::SpaceKind,
};

/// Record a chat `id` as a new space is, and its number.
fn record(
    tx: &Transaction<'_>,
    records: &dyn shoal_spaces::daemon::Records,
    id: &str,
) -> Result<SpaceNumber> {
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
    spaces::records::record(tx, records, &new)
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
    let records = manager.spaces.records.clone();
    let numbers = (manager.store)
        .write(move |tx| {
            let alone = record(tx, &*records, "alone")?;
            keep_spares(tx, &spares(&[1, 4]))?;
            // A spare a space here holds is passed over, and once none is
            // left a space takes the number above the highest here.
            let numbers = [
                alone,
                record(tx, &*records, "spare")?,
                record(tx, &*records, "above")?,
            ];
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
async fn a_renumbered_space_takes_its_threads_and_their_waits_along() -> Result<()> {
    let (root, manager) = test_support::manager().await;
    let path = test_support::repository(root.path(), "repo");
    let repo = (manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None))
    .await?;
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let (asker, other) = (create("asker").await?, create("other").await?);
    let selector = SpaceSelector::id(&asker.id);
    let asked = test_support::ask(&manager, &asker.id, "Which database?").await;
    let (item, after) = (asked.number.item, asked.posts[0].id);
    let waiter = tokio::spawn({
        let (manager, selector) = (manager.clone(), selector.clone());
        async move {
            manager
                .coordination
                .await_post(&*manager, None, &selector, item, after)
                .await
        }
    });
    // An issue proposal belongs to the repository and names its space in
    // its record.
    let (repository, proposer) = (repo.id.clone(), asker.id.clone());
    (manager.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO access_requests(id, repository_id, target_key, name, record,
                        space_number, number)
                    VALUES ('proposal', ?1, 'issue', 'issue', json_object('space_id', ?2), 1, 2)",
                params![repository, proposer],
            )?;
            Ok(())
        })
        .await?;
    let renumbering = |space: &SpaceId| Numbering {
        spares: Vec::new(),
        renumbered: vec![Renumbering {
            space: space.clone(),
            number: SpaceNumber::new(4),
        }],
    };
    manager
        .hub
        .take_numbering(&*manager, renumbering(&asker.id))
        .await?;
    // A number a space here holds is not taken.
    manager
        .hub
        .take_numbering(&*manager, renumbering(&other.id))
        .await?;
    let mut numbers = Vec::new();
    for id in [&asker.id, &other.id] {
        numbers.push(
            manager
                .spaces
                .space(&SpaceSelector::id(id))
                .await?
                .number
                .get(),
        );
    }
    assert_eq!(numbers, [4, 2]);
    let proposal = (manager.store).read(|tx| {
        let number = "SELECT space_number FROM access_requests WHERE id='proposal'";
        Ok(tx.query_row(number, [], |row| row.get::<_, u32>(0))?)
    });
    assert_eq!(proposal.await?, 4);
    let renumbered = ItemNumber {
        space: SpaceNumber::new(4),
        item,
    };
    test_support::answer(&manager, renumbered, "Postgres").await;
    let answered = waiter.await??;
    assert_eq!(answered.posts.last().unwrap().post.text, "Postgres");
    let notifications = manager.notifications.list(false, 50).await?;
    let notice = (notifications.iter())
        .find(|notification| notification.kind == AlertKind::SpaceRenumbered)
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
            .hub
            .keep_imported_number(&SpaceId::from(id), number)
            .await?;
    }
    let records = manager.spaces.records.clone();
    let numbers = (manager.store)
        .write(move |tx| {
            let numbers = [
                record(tx, &*records, "here")?,
                record(tx, &*records, "moved")?,
                record(tx, &*records, "taken")?,
            ];
            Ok(numbers.map(SpaceNumber::get))
        })
        .await?;
    // A number a space here holds is not kept.
    assert_eq!(numbers, [1, 5, 2]);
    Ok(())
}

#[tokio::test]
async fn a_machine_reserves_the_numbers_it_holds_without_a_space() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let records = manager.spaces.records.clone();
    let reserved = (manager.store)
        .write(move |tx| {
            let (gone, here) = (
                record(tx, &*records, "gone")?,
                record(tx, &*records, "here")?,
            );
            // Issue proposals outlive the space that made them, whose number
            // they keep.
            for (key, space, number) in [("of-gone", "gone", gone), ("of-here", "here", here)] {
                let checkout = std::path::Path::new("/").join(key);
                let repository = shoal_protocol::test_support::repository(key, &checkout);
                seeds::repository(tx, &repository)?;
                tx.execute(
                    "INSERT INTO access_requests(id, repository_id, target_key, name, record,
                            space_number, number)
                        VALUES (?1, ?1, 'issue', 'issue', json_object('space_id', ?2), ?3, 1)",
                    params![key, space, number],
                )?;
            }
            spaces::records::remove(tx, &SpaceId::from("gone"))?;
            super::super::HubRecords.reserved_numbers(tx)
        })
        .await?;
    assert_eq!(reserved, [SpaceNumber::new(1)]);
    Ok(())
}
