//! Chats without a repository, which run in `root_dir`.
use crate::{
    daemon::{chats::NewChat, workspace::Manager},
    model::Chat,
    test_support::{ProcessGroup, manager},
};

/// Start the chat `name` without a repository, below `parent` when given,
/// running as the returned process.
async fn outside(manager: &Manager, name: &str, parent: Option<&str>) -> (Chat, ProcessGroup) {
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = NewChat {
        name: name.into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: parent.map(str::to_owned),
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    (chat, process)
}

#[tokio::test]
async fn a_chat_without_a_repository_keeps_its_temporary_directory_in_root_dir() {
    let (_temp, manager) = manager().await;
    let (master, _process) = outside(&manager, "master", None).await;
    assert_eq!(master.repository_id, None);
    let tmp_dir = master.tmp_dir.clone().unwrap();
    assert!(tmp_dir.is_dir(), "{}", tmp_dir.display());
    let root = manager.config().root_dir(&manager.paths).unwrap();
    assert!(tmp_dir.starts_with(root), "{}", tmp_dir.display());
    let listed = manager.recorded_chat(&master.id).await.unwrap().unwrap();
    assert_eq!(listed.tmp_dir, Some(tmp_dir.clone()));
    manager.remove_chat(&master.id, None).await.unwrap();
    assert!(!tmp_dir.exists(), "{}", tmp_dir.display());
}
