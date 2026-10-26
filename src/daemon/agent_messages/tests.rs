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
    let changed = manager.agent_messages_changed.subscribe();
    for _ in 0..2 {
        manager
            .send_agent_message(&workspace.name, "Stop the dev server".into())
            .await
            .unwrap();
    }
    assert!(changed.has_changed().unwrap());
    manager
        .send_agent_message(&workspace.name, "Rebase onto main".into())
        .await
        .unwrap();
    manager
        .send_agent_message(&workspace.name, "Move:\n\tabc123 to api".into())
        .await
        .unwrap();
    for rejected in [
        "\x1b]9;spoof\x07".to_owned(),
        "x".repeat(crate::validate::MAX_AGENT_MESSAGE_LEN + 1),
    ] {
        assert!(
            manager
                .send_agent_message(&workspace.name, rejected)
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
        .send_agent_message(&workspace.name, "Stop the dev server".into())
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
            .send_agent_message(&workspace.name, format!("message {n}"))
            .await
            .unwrap();
    }
    assert!(
        manager
            .send_agent_message(&workspace.name, "one more".into())
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
    };
    assert!(
        scope::authorize(&manager, Some("scope"), &mut method)
            .await
            .is_err()
    );
}
