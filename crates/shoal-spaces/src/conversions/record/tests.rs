use crate::conversions::record::ChatRow;
use crate::stand_in::Fixture;
use shoal_store::test_support::seeds;

#[test]
fn a_chat_row_keeps_its_scope_token_typed_and_out_of_its_debug() {
    let row: ChatRow =
        serde_json::from_value(serde_json::json!({"id": "c", "scope_token": "secret-token"}))
            .unwrap();

    let shown = format!("{row:?}");
    let stored = serde_json::to_value(&row).unwrap();

    assert!(!shown.contains("secret-token"), "{shown}");
    assert_eq!(stored["scope_token"], "secret-token");
    assert_eq!(stored["id"], "c");
}

#[tokio::test]
async fn a_workspace_becoming_a_chat_keeps_its_threads_and_requests_to_take_actions() {
    let manager = Fixture::new().await;
    let chat = crate::conversions::pending::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    let records = manager.spaces.records.clone();
    let kept: Vec<(String, String)> = (manager.store)
        .write(move |tx| {
            seeds::ready_workspace(tx, "repo", "w", "fix")?;
            tx.execute_batch(
                "INSERT INTO threads(space_number,number,space_id,kind,name,created_at,opener,
                         opener_id)
                     VALUES (1,1,'w','question','Which?',1,'space','w');
                 INSERT INTO access_requests(id,space_id,target_key,name,record,space_number,number)
                     VALUES ('action','w','action/merge','merge','{}',1,2),
                         ('port','w','port/web','web','{}',1,3);",
            )?;
            crate::conversions::record::replace_workspace(tx, &*records, &"w".into(), &chat)?;
            let mut kept: Vec<(String, String)> = tx
                .prepare(
                    "SELECT 'thread',space_id FROM threads
                     UNION ALL SELECT id,space_id FROM access_requests",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            kept.sort();
            Ok(kept)
        })
        .await
        .unwrap();
    let owned = |item: &str| (item.to_owned(), "w".to_owned());
    // A request for the worktree's port goes with it.
    assert_eq!(kept, [owned("action"), owned("thread")]);
}

#[tokio::test]
async fn a_chat_s_row_kept_by_a_conversion_inserts_it_again_as_it_was() {
    let manager = Fixture::new().await;
    let (kept, before, after) = (manager.store)
        .write(|tx| {
            let chat =
                shoal_protocol::test_support::chat("c", None, "chat", std::path::Path::new("/"));
            seeds::chat(tx, &chat)?;
            let id = "c".into();
            type Columns = (String, Option<String>, Option<String>);
            let read = |tx: &rusqlite::Transaction<'_>| -> rusqlite::Result<Columns> {
                tx.query_row(
                    "SELECT id,workspace_id,root_dir FROM chats WHERE id='c'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            };
            let before = read(tx)?;
            let kept = super::chat_row(tx, &id)?;
            tx.execute("DELETE FROM chats WHERE id='c'", [])?;
            super::insert_chat(tx, &kept)?;
            Ok((serde_json::to_value(&kept)?, before, read(tx)?))
        })
        .await
        .unwrap();

    assert_eq!(after, before);
    assert_eq!(kept["name"], "c");
    assert_eq!(kept["number"], 1);
    assert_eq!(kept["mode"], "chat");
    assert_eq!(kept["root_dir"], serde_json::Value::Null);
}
