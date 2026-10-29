use crate::{
    daemon::{
        scope::{self, Caller},
        workspace::Manager,
    },
    protocol::AgentMethod,
    test_support::{manager, repository},
};

async fn workspace(
    temp: &tempfile::TempDir,
    manager: &Manager,
    name: &str,
) -> crate::model::Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn messages_wait_until_delivered_and_repeats_collapse() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let changed = manager.signals.watch_agent_messages();
    for _ in 0..2 {
        manager
            .send_agent_message(&workspace.name, "Stop the dev server".into(), Vec::new())
            .await
            .unwrap();
    }
    assert!(changed.has_changed().unwrap());
    manager
        .send_agent_message(&workspace.name, "Rebase onto main".into(), Vec::new())
        .await
        .unwrap();
    manager
        .send_agent_message(&workspace.name, "Move:\n\tabc123 to api".into(), Vec::new())
        .await
        .unwrap();
    for rejected in [
        "\x1b]9;spoof\x07".to_owned(),
        "x".repeat(crate::validate::MAX_AGENT_MESSAGE_LEN + 1),
    ] {
        assert!(
            manager
                .send_agent_message(&workspace.name, rejected, Vec::new())
                .await
                .is_err()
        );
    }
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    let texts: Vec<_> = messages.iter().map(|m| m.message.as_str()).collect();
    assert_eq!(
        texts,
        [
            "Stop the dev server",
            "Rebase onto main",
            "Move:\n\tabc123 to api"
        ]
    );
    manager
        .mark_agent_messages_delivered(&workspace.name, vec![messages[0].id])
        .await
        .unwrap();
    let remaining = manager.agent_messages(&workspace.name).await.unwrap();
    assert_eq!(remaining, messages[1..]);
    // A delivered message may be sent again.
    manager
        .send_agent_message(&workspace.name, "Stop the dev server".into(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        manager.agent_messages(&workspace.name).await.unwrap().len(),
        3
    );
}

#[tokio::test]
async fn unread_messages_are_bounded() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for n in 0..super::MAX_UNREAD {
        manager
            .send_agent_message(&workspace.name, format!("message {n}"), Vec::new())
            .await
            .unwrap();
    }
    assert!(
        manager
            .send_agent_message(&workspace.name, "one more".into(), Vec::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn workspace_processes_read_their_own_messages_but_cannot_send() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
        for mut method in [
            AgentMethod::AgentMessages {
                workspace: target.clone(),
            },
            AgentMethod::MarkAgentMessagesDelivered {
                workspace: target.clone(),
                ids: Vec::new(),
            },
        ] {
            assert_eq!(
                scope::authorize(&manager, Some("scope"), &mut method)
                    .await
                    .is_ok(),
                allowed
            );
        }
    }
    let mut method = AgentMethod::SendAgentMessage {
        workspace: workspace.name.clone(),
        message: "hello".into(),
        attachments: Vec::new(),
    };
    assert!(
        scope::authorize(&manager, Some("scope"), &mut method)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn attached_files_are_copied_beside_the_workspaces_until_removal() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    for _ in 0..2 {
        manager
            .send_agent_message(&workspace.name, "See the log".into(), vec![file.clone()])
            .await
            .unwrap();
    }
    manager
        .send_agent_message(&workspace.name, String::new(), vec![file.clone()])
        .await
        .unwrap();
    std::fs::write(&file, "changed").unwrap();
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    let [attached, alone] = messages.as_slice() else {
        panic!("{messages:?}");
    };
    assert_eq!(alone.message, "");
    let [copy] = attached.attachments.as_slice() else {
        panic!("{attached:?}");
    };
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "panic");
    assert_eq!(copy.file_name(), file.file_name());
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let workspaces_dir = repo.workspaces_dir.as_deref().unwrap();
    assert!(copy.starts_with(workspaces_dir.join(".attachments").join(&workspace.id)));

    manager
        .mark_agent_messages_delivered(&workspace.name, vec![attached.id])
        .await
        .unwrap();
    assert!(copy.exists(), "a delivered message keeps its files");
    manager
        .remove_workspace(
            &workspace.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!copy.exists());
}

#[tokio::test]
async fn a_refused_message_leaves_no_copies() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for n in 0..super::MAX_UNREAD {
        manager
            .send_agent_message(&workspace.name, format!("message {n}"), Vec::new())
            .await
            .unwrap();
    }
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    assert!(
        manager
            .send_agent_message(&workspace.name, "See the log".into(), vec![file])
            .await
            .is_err()
    );
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let attachments = repo.workspaces_dir.unwrap().join(".attachments");
    assert!(!attachments.join(&workspace.id).exists());
}

#[tokio::test]
async fn attachments_must_be_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager, "receiver").await;
    for refused in [
        temp.path().to_path_buf(),
        temp.path().join("missing.png"),
        "relative.png".into(),
    ] {
        let error = manager
            .send_agent_message(&workspace.name, "See".into(), vec![refused.clone()])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains(&*refused.to_string_lossy()),
            "{error:#}"
        );
    }
    assert!(
        manager
            .send_agent_message(&workspace.name, String::new(), Vec::new())
            .await
            .is_err()
    );
    assert!(
        manager
            .agent_messages(&workspace.name)
            .await
            .unwrap()
            .is_empty()
    );
}
