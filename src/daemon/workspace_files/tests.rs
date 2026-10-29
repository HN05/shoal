use super::FileSource;
use crate::{
    daemon::workspace::Manager,
    test_support::{manager, repository},
};

async fn workspace(temp: &tempfile::TempDir, manager: &Manager) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn files_sent_with_messages_are_listed_until_removal() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    assert!(
        manager
            .workspace_files(&workspace.name)
            .await
            .unwrap()
            .is_empty()
    );
    // Sending the same file again keeps one record of its copy.
    for _ in 0..2 {
        manager
            .send_agent_message(&workspace.name, "See the log".into(), vec![file.clone()])
            .await
            .unwrap();
    }
    let files = manager.workspace_files(&workspace.name).await.unwrap();
    let [listed] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(listed.name, "crash.log");
    assert_eq!(listed.bytes, 5);
    assert_eq!(listed.source, FileSource::Message);
    assert_eq!(listed.workspace, "receiver");
    assert_eq!(std::fs::read_to_string(&listed.path).unwrap(), "panic");

    manager
        .remove_workspace(
            &workspace.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!listed.path.exists());
    let left = manager
        .store
        .run(|db| {
            Ok(
                db.query_row("SELECT COUNT(*) FROM workspace_files", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn added_files_need_no_message() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("config.toml");
    std::fs::write(&file, "port = 1").unwrap();
    let added = manager
        .add_workspace_files(&workspace.name, vec![file.clone()])
        .await
        .unwrap();
    let [copy] = added.as_slice() else {
        panic!("{added:?}");
    };
    assert_eq!(copy.source, FileSource::Added);
    assert_eq!(std::fs::read_to_string(&copy.path).unwrap(), "port = 1");
    // Adding it again keeps the copy; sending it is a separate copy.
    manager
        .add_workspace_files(&workspace.name, vec![file.clone()])
        .await
        .unwrap();
    manager
        .send_agent_message(&workspace.name, String::new(), vec![file])
        .await
        .unwrap();
    let files = manager.workspace_files(&workspace.name).await.unwrap();
    let sources: Vec<_> = files.iter().map(|file| file.source).collect();
    assert_eq!(sources, [FileSource::Added, FileSource::Message]);
    assert!(
        manager
            .agent_messages(&workspace.name)
            .await
            .unwrap()
            .iter()
            .all(|message| message.attachments.len() == 1),
        "adding sends no message"
    );
}
