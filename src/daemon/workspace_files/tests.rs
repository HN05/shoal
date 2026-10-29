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

#[tokio::test]
async fn deleting_by_name_keeps_the_other_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (log, shot) = (temp.path().join("crash.log"), temp.path().join("shot.png"));
    std::fs::write(&log, "panic").unwrap();
    std::fs::write(&shot, "image").unwrap();
    manager
        .send_agent_message(&workspace.name, "See".into(), vec![log.clone(), shot])
        .await
        .unwrap();
    let error = manager
        .remove_workspace_files(&workspace.name, Some("missing.txt".into()))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no attached file named"),
        "{error:#}"
    );
    let deleted = manager
        .remove_workspace_files(&workspace.name, Some("crash.log".into()))
        .await
        .unwrap();
    assert!(!deleted[0].path.exists());
    let left = manager.workspace_files(&workspace.name).await.unwrap();
    let [kept] = left.as_slice() else {
        panic!("{left:?}");
    };
    assert_eq!(kept.name, "shot.png");
    assert!(kept.path.exists());

    // Sending the same files again copies the deleted one anew.
    manager
        .send_agent_message(
            &workspace.name,
            "See".into(),
            vec![log, temp.path().join("shot.png")],
        )
        .await
        .unwrap();
    assert!(deleted[0].path.exists());
    assert_eq!(
        manager
            .workspace_files(&workspace.name)
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn deleting_every_workspaces_files_spares_active_ones() {
    let (temp, manager) = manager().await;
    let idle = workspace(&temp, &manager).await;
    let active = manager
        .create_workspace(&idle.repository_id, "active".into(), None, None, None)
        .await
        .unwrap();
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    for workspace in [&idle, &active] {
        manager
            .add_workspace_files(&workspace.name, vec![file.clone()])
            .await
            .unwrap();
    }
    let id = active.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES ('agent',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let deleted = manager.remove_idle_files().await.unwrap();
    let [gone] = deleted.as_slice() else {
        panic!("{deleted:?}");
    };
    assert_eq!(gone.workspace, "receiver");
    assert!(!gone.path.exists());
    assert_eq!(
        manager.workspace_files(&active.name).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn results_outlive_removal_as_long_as_its_record() {
    use crate::daemon::{
        notifications::NotificationKind,
        scope::{self, Caller},
    };
    use crate::protocol::FileMethod;
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager).await;
    let child = manager
        .split_workspace(&parent.id, "audit".into(), None)
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    let completion = manager
        .mark_done(&child.id, false, false, vec![report])
        .await
        .unwrap();
    let [result] = completion.results.as_slice() else {
        panic!("{completion:?}");
    };
    assert_eq!(std::fs::read_to_string(result).unwrap(), "No leaks");
    assert!(
        manager
            .notifications(true, 50)
            .await
            .unwrap()
            .iter()
            .any(|n| n.kind == NotificationKind::WorkspaceDone
                && n.message
                    .ends_with("; results in shoal files audit: report.md")),
    );

    manager
        .remove_workspace(
            &child.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    let files = manager.workspace_files("audit").await.unwrap();
    let [kept] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(kept.source, FileSource::Result);
    assert!(kept.expires_at.is_some(), "{kept:?}");
    assert!(result.exists());

    // The parent's processes reach the removed child's results; others do not.
    let sibling = manager
        .create_workspace(&parent.repository_id, "sibling".into(), None, None, None)
        .await
        .unwrap();
    for (token, workspace, allowed) in [("parent", &parent, true), ("sibling", &sibling, false)] {
        manager
            .executions
            .issue_scope(
                token.into(),
                Caller {
                    workspace_id: workspace.id.clone(),
                    execution: None,
                },
            )
            .await;
        let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
            workspace: "audit".into(),
        });
        let authorized = scope::authorize(&manager, Some(token), &mut list).await;
        assert_eq!(authorized.is_ok(), allowed, "{token}");
    }

    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM removed_workspaces WHERE workspace_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    assert!(!result.exists());
    assert!(manager.workspace_files("audit").await.is_err());
}
