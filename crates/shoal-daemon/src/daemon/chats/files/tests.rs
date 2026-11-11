use crate::daemon::test_support::{manager, repository};

#[tokio::test]
async fn chat_files_move_from_the_chat_name_to_its_id_and_others_go() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let directory = manager.paths.state.join("chats");
    std::fs::create_dir_all(&directory).unwrap();
    let write = |file: &str| std::fs::write(directory.join(file), file).unwrap();
    // Files from before chats' files were named after their IDs.
    write("talk.log");
    write("talk.prompt.md");
    // An ended chat's file.
    write("ended.log");
    manager.rekey_chat_files().await.unwrap();

    let files = manager.paths.chat_files(&chat);
    let read = |extension: &str| std::fs::read_to_string(files.with_extension(extension));
    assert_eq!(read("log").unwrap(), "talk.log");
    assert_eq!(read("prompt.md").unwrap(), "talk.prompt.md");
    let mut left: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [format!("{}.log", chat.id), format!("{}.prompt.md", chat.id)]
    );
    // Files already named after the chat stay.
    manager.rekey_chat_files().await.unwrap();
    assert_eq!(read("log").unwrap(), "talk.log");
}

#[tokio::test]
async fn files_of_a_name_the_upgrade_cut_go_to_the_chat_it_recorded_alone() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let start = |name: crate::client::space_names::SpaceName| {
        let (manager, repo) = (&manager, repo.id.clone());
        async move {
            (manager.start_chat(
                repo.into(),
                name,
                "claude".into(),
                std::process::id(),
                None,
                false,
            ))
            .await
            .unwrap()
        }
    };
    // The upgrade cut the chat's name, recording the one its files took;
    // an ended chat's name cuts to the same.
    let (kept, ended) = (
        format!("{}a", "x".repeat(63)),
        format!("{}b", "x".repeat(63)),
    );
    let cut = start(crate::client::space_names::SpaceName::derive(&kept, "chat")).await;
    let (id, name) = (cut.id.clone(), kept.clone());
    (manager.store)
        .write(move |tx| {
            tx.execute(
                "INSERT INTO chat_legacy_names(chat_id,name) VALUES (?1,?2)",
                rusqlite::params![id, name],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Two chats that share a name, as names now may.
    start("shared".into()).await;
    start("shared".into()).await;
    let directory = manager.paths.state.join("chats");
    std::fs::create_dir_all(&directory).unwrap();
    let write = |file: &str, text: &str| std::fs::write(directory.join(file), text).unwrap();
    write(&format!("{kept}.log"), "kept");
    write(&format!("{ended}.log"), "ended");
    write("shared.log", "shared");
    // A file the chat has under its ID already is not replaced.
    let files = manager.paths.chat_files(&cut);
    std::fs::write(files.with_extension("prompt.md"), "current").unwrap();
    write(&format!("{kept}.prompt.md"), "earlier");
    manager.rekey_chat_files().await.unwrap();

    let read = |extension: &str| std::fs::read_to_string(files.with_extension(extension)).unwrap();
    assert_eq!(read("log"), "kept");
    assert_eq!(read("prompt.md"), "current");
    assert!(!directory.join(format!("{ended}.log")).exists());
    // Neither chat sharing the name is its owner for sure, so it stays.
    assert!(directory.join("shared.log").exists());
}
