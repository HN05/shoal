use super::*;
use crate::client::space_ids::SpaceId;

fn spaces(db: &Connection) -> Result<()> {
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES
            ('first','repo','first','/first','first','ready');
        INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at)
            VALUES ('chat','repo','planning','claude',1,'b',1);",
    )?;
    Ok(())
}

fn question(db: &Connection, space: &str, number: ItemNumber) -> Result<()> {
    let (workspace, chat) = match space {
        "chat" => (None, Some(space)),
        _ => (Some(space), None),
    };
    db.execute(
        "INSERT INTO questions(space_number,number,workspace_id,chat_id,created_at,question,
                recommendation,choices)
            VALUES (?1,?2,?3,?4,1,'Which?','This','[]')",
        params![number.space, number.item, workspace, chat],
    )?;
    Ok(())
}

fn request(db: &Connection, key: &str, number: ItemNumber) -> Result<()> {
    db.execute(
        "INSERT INTO access_requests(id,workspace_id,target_key,name,record,space_number,number)
            VALUES (?1,'first','port/web',?1,'{}',?2,?3)",
        params![key, number.space, number.item],
    )?;
    Ok(())
}

#[tokio::test]
async fn questions_and_requests_share_one_sequence_per_space() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    manager
        .store
        .run(|db| {
            spaces(db)?;
            let first = next(db, &SpaceId::from("first"))?;
            assert_eq!(first, ItemNumber { space: 1, item: 1 });
            question(db, "first", first)?;
            let second = next(db, &SpaceId::from("first"))?;
            assert_eq!(second, ItemNumber { space: 1, item: 2 });
            request(db, "web", second)?;
            assert_eq!(next(db, &SpaceId::from("first"))?.item, 3);
            // Another space counts from 1 on its own.
            assert_eq!(
                next(db, &SpaceId::from("chat"))?,
                ItemNumber { space: 2, item: 1 }
            );
            // A deleted item frees its number for the next.
            db.execute("DELETE FROM questions", [])?;
            assert_eq!(next(db, &SpaceId::from("first"))?.item, 1);
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_number_of_the_other_kind_names_the_right_command() -> Result<()> {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    manager
        .store
        .run(|db| {
            spaces(db)?;
            let asked = ItemNumber { space: 1, item: 1 };
            let requested = ItemNumber { space: 1, item: 2 };
            question(db, "first", asked)?;
            request(db, "web", requested)?;
            let refusal =
                |kind, number| -> Result<String> { Ok(unknown(db, kind, number)?.to_string()) };
            assert!(refusal(ItemKind::Approval, asked)?.contains("shoal questions"));
            assert!(refusal(ItemKind::Question, requested)?.contains("shoal approvals"));
            let absent = ItemNumber { space: 1, item: 3 };
            assert!(refusal(ItemKind::Question, absent)?.starts_with("unknown"));
            Ok(())
        })
        .await
}
