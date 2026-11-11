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
async fn files_of_a_name_the_upgrade_cut_move_unless_chats_share_it() {
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
    // Its name as the upgrade cut it from the one its files took.
    let long = "x".repeat(64);
    let cut = start(crate::client::space_names::SpaceName::derive(&long, "chat")).await;
    // Two chats that share a name, as names now may.
    start("shared".into()).await;
    start("shared".into()).await;
    let directory = manager.paths.state.join("chats");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(format!("{long}.log")), "long").unwrap();
    std::fs::write(directory.join("shared.log"), "shared").unwrap();
    manager.rekey_chat_files().await.unwrap();

    let files = manager.paths.chat_files(&cut);
    assert_eq!(
        std::fs::read_to_string(files.with_extension("log")).unwrap(),
        "long"
    );
    // Neither chat sharing the name is its owner for sure, so it stays.
    assert!(directory.join("shared.log").exists());
}
