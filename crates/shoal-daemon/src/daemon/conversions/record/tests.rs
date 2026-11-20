use super::ChatRow;

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
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let chat = super::ConvertedChat {
        agent: "claude".into(),
        execution: shoal_ids::execution_ids::ExecutionId::generate(),
        origin: None,
        mode: crate::model::chat_mode(),
        narrowing: Default::default(),
    };
    let kept: Vec<(String, String)> = (manager.store)
        .write(move |tx| {
            tx.execute_batch(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
                 INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','fix');
                 INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir) VALUES ('w','/work','fix','ready','abc','/admin');
                 INSERT INTO threads(space_number,number,space_id,kind,name,created_at,opener)
                     VALUES (1,1,'w','question','Which?',1,'space');
                 INSERT INTO access_requests(id,space_id,target_key,name,record,space_number,number)
                     VALUES ('action','w','action/merge','merge','{}',1,2),
                         ('port','w','port/web','web','{}',1,3);",
            )?;
            super::replace_workspace(tx, &"w".into(), &chat)?;
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
