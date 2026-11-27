use super::*;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::test_support;
use shoal_store::test_support::seeds;
use std::path::Path;

fn spaces(db: &Connection) -> Result<()> {
    seeds::ready_workspace(db, "repo", "first", "first")?;
    let chat = test_support::chat("chat", Some("repo"), "chat", Path::new("/repo"));
    seeds::chat(db, &chat)?;
    Ok(())
}

fn thread(db: &Connection, space: &str, number: ItemNumber) -> Result<()> {
    db.execute(
        "INSERT INTO threads(space_number,number,space_id,kind,name,created_at,opener,opener_id)
            VALUES (?1,?2,?3,'question','Which?',1,'space',?3)",
        params![number.space, number.item, space],
    )?;
    Ok(())
}

fn request(db: &Connection, key: &str, number: ItemNumber) -> Result<()> {
    seeds::access_request(db, key, &SpaceId::from("first"), ("port/web", key), number)
}

#[tokio::test]
async fn threads_and_requests_share_one_sequence_per_space() -> Result<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(temp.path().join("state.db")).await?;
    store
        .run(|db| {
            spaces(db)?;
            let first = next(db, &SpaceId::from("first"))?;
            assert_eq!(
                first,
                ItemNumber {
                    space: 1.into(),
                    item: 1
                }
            );
            thread(db, "first", first)?;
            let second = next(db, &SpaceId::from("first"))?;
            assert_eq!(
                second,
                ItemNumber {
                    space: 1.into(),
                    item: 2
                }
            );
            request(db, "web", second)?;
            assert_eq!(next(db, &SpaceId::from("first"))?.item, 3);
            // Another space counts from 1 on its own.
            assert_eq!(
                next(db, &SpaceId::from("chat"))?,
                ItemNumber {
                    space: 2.into(),
                    item: 1
                }
            );
            // A deleted item frees its number for the next.
            db.execute("DELETE FROM threads", [])?;
            assert_eq!(next(db, &SpaceId::from("first"))?.item, 1);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_number_of_the_other_kind_names_the_right_command() -> Result<()> {
    let temp = tempfile::tempdir().unwrap();
    let store = shoal_store::test_support::unsynced_store(temp.path().join("state.db")).await?;
    store
        .run(|db| {
            spaces(db)?;
            let asked = ItemNumber {
                space: 1.into(),
                item: 1,
            };
            let requested = ItemNumber {
                space: 1.into(),
                item: 2,
            };
            thread(db, "first", asked)?;
            request(db, "web", requested)?;
            let refusal =
                |kind, number| -> Result<String> { Ok(unknown(db, kind, number)?.to_string()) };
            assert!(refusal(ItemKind::Approval, asked)?.contains("shoal view thread"));
            assert!(refusal(ItemKind::Thread, requested)?.contains("shoal approvals"));
            let absent = ItemNumber {
                space: 1.into(),
                item: 3,
            };
            assert!(refusal(ItemKind::Thread, absent)?.starts_with("unknown"));
            Ok(())
        })
        .await
}
