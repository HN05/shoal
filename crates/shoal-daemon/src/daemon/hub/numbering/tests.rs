use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;

use super::*;
use crate::{config::permissions::PermissionsConfig, daemon::spaces, model::SpaceKind};

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
            let joined = next(tx)?;
            tx.execute("UPDATE hub_membership SET revoked=1", [])?;
            forget_unless_joined(tx)?;
            Ok((joined, next(tx)?))
        })
        .await?;
    assert_eq!(numbered, (Some(SpaceNumber::new(7)), None));
    Ok(())
}
