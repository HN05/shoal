use super::*;
use shoal_ids::space_ids::SpaceId;

fn spaces(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        INSERT INTO spaces(id,kind,repository_id,name)
            VALUES ('first','workspace','repo','first');
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('first','/first','first','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name,mode)
            VALUES ('chat','chat','repo','planning','chat');
        INSERT INTO chats(id)
            VALUES ('chat');",
    )?;
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
    db.execute(
        "INSERT INTO access_requests(id,space_id,target_key,name,record,space_number,number)
            VALUES (?1,'first','port/web',?1,'{}',?2,?3)",
        params![key, number.space, number.item],
    )?;
    Ok(())
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
