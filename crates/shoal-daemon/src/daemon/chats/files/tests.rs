use crate::daemon::test_support::{manager, repository};
use crate::paths::StateFiles;

#[tokio::test]
async fn chat_files_no_chat_owns_go() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let directory = manager.paths.state.join("chats");
    std::fs::create_dir_all(&directory).unwrap();
    let log = manager.paths.chat_files(&chat).with_extension("log");
    std::fs::write(&log, "current").unwrap();
    let write = |file: &str| std::fs::write(directory.join(file), file).unwrap();
    // An ended chat's file, and a first message earlier versions kept.
    write("ended.log");
    write(&format!("{}.prompt.md", chat.id));
    manager.remove_unowned_chat_files().await.unwrap();

    assert_eq!(std::fs::read_to_string(&log).unwrap(), "current");
    let left: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(left, std::slice::from_ref(&log));
}
