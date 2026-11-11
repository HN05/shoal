use crate::client::{progress::ProgressChange, space_names::SpaceSelector};
use crate::{
    config::modes::ModeName,
    daemon::{
        conversions::{Converting, Pending},
        events::{ChatEventKind, EventItem, EventKind},
        test_support::{chat, manager, repository},
        workspace::Manager,
    },
    model::Repository,
};
use std::path::Path;

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// The kinds of the journal's events about the space `id`.
async fn kinds(manager: &Manager, id: &crate::client::space_ids::SpaceId) -> Vec<String> {
    (manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter())
    .filter_map(|item| match item {
        EventItem::Event(event)
            if [&event.details.workspace_id, &event.details.chat_id]
                .iter()
                .any(|space| space.as_ref() == Some(id)) =>
        {
            Some(event.details.kind.to_string())
        }
        EventItem::Chat(event) if event.chat.id == *id => Some(event.kind.to_string()),
        _ => None,
    })
    .collect()
}

#[tokio::test]
async fn a_chat_becomes_a_workspace_under_its_id_number_name_and_parent() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let (lead, _lead) = chat(&manager, "lead", "chat", Some(&repo), None).await;
    let (talk, _talk) = chat(&manager, "talk", "worker", Some(&repo), Some(&lead.id)).await;
    let status = ProgressChange::Status(Some("Reproduced".into()));
    (manager.set_progress(&SpaceSelector::id(&talk.id), status))
        .await
        .unwrap();
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
    };

    let workspace = manager
        .create_converted_workspace(converting, "fix-login".into(), None, Default::default())
        .await
        .unwrap();

    assert_eq!(
        (&workspace.id, workspace.number, &workspace.name),
        (&talk.id, talk.number, &talk.name)
    );
    assert_eq!(workspace.branch.as_deref(), Some("fix-login"));
    assert!(
        workspace.path.ends_with("fix-login"),
        "{:?}",
        workspace.path
    );
    assert_eq!(workspace.mode, Some(ModeName::DEFAULT));
    assert_eq!(workspace.parent.map(|parent| parent.id), Some(lead.id));
    let progress = workspace.progress.and_then(|progress| progress.status);
    assert_eq!(progress.as_deref(), Some("Reproduced"));
    assert!(manager.chat_by_id(&talk.id).await.unwrap().is_none());
    let id = talk.id.clone();
    let pending = (manager.store)
        .run(move |db| crate::daemon::conversions::pending(db, &id))
        .await
        .unwrap();
    let Some(Pending::Workspace { chat, .. }) = pending else {
        panic!("expected the chat's restart to wait: {pending:?}");
    };
    assert_eq!(chat.agent().unwrap().pid, talk.pid);
    let kinds = kinds(&manager, &talk.id).await;
    assert!(
        kinds.contains(&EventKind::Converted.to_string()),
        "{kinds:?}"
    );
    for gone in [
        EventKind::Created.to_string(),
        ChatEventKind::Ended.to_string(),
    ] {
        assert!(!kinds.contains(&gone), "{kinds:?}");
    }
}

#[tokio::test]
async fn a_worktree_that_cannot_be_made_gives_the_chat_back() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let (talk, _talk) = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
    };

    let base = Some("no-such-ref".to_owned());
    let created = manager
        .create_converted_workspace(converting, "fix-login".into(), base, Default::default())
        .await;

    assert!(created.is_err());
    let restored = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    assert_eq!(
        (restored.number, restored.pid, &restored.name),
        (talk.number, talk.pid, &talk.name)
    );
    assert!(manager.workspace_by_id(&talk.id).await.is_err());
    let token = manager.chat_scope(&talk.id).await.unwrap();
    assert!(manager.caller(&token).await.unwrap().is_some());
    // It was the workspace's once its record went in, and is the chat again.
    let converted = EventKind::Converted.to_string();
    let kinds = kinds(&manager, &talk.id).await;
    assert_eq!(
        kinds.iter().filter(|kind| **kind == converted).count(),
        2,
        "{kinds:?}"
    );
}

#[tokio::test]
async fn a_branch_another_workspace_holds_is_refused_before_anything_changes() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let holder = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let (talk, _talk) = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
    };

    let error = manager
        .create_converted_workspace(converting, "fix-login".into(), None, Default::default())
        .await
        .unwrap_err();

    assert!(
        format!("{error:#}").contains(holder.name.as_str()),
        "{error:#}"
    );
    assert!(manager.chat_by_id(&talk.id).await.unwrap().is_some());
}

#[tokio::test]
async fn the_chats_open_questions_stay_with_the_space() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let (talk, _talk) = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let selector = SpaceSelector::id(&talk.id);
    let ask = crate::client::questions::Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    let attacher = crate::daemon::workspace_files::Attacher::User;
    let asked = (manager.ask_question(&selector, ask, Vec::new(), &attacher))
        .await
        .unwrap();
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
    };

    manager
        .create_converted_workspace(converting, "fix-login".into(), None, Default::default())
        .await
        .unwrap();

    let listed = manager.questions(Some(&selector)).await.unwrap().questions;
    let numbers: Vec<_> = (listed.iter())
        .filter(|question| question.is_open())
        .map(|question| question.number)
        .collect();
    assert_eq!(numbers, [asked.number]);
}

#[tokio::test]
async fn a_remote_branch_named_with_its_remote_opens_as_that_branch() {
    let (root, manager) = manager().await;
    let remote = repository(root.path(), "remote");
    crate::daemon::test_support::git(&remote, &["branch", "topic"]);
    let repo = registered(root.path(), &manager).await;
    let checkout = repo.path.to_str().unwrap();
    let remote_path = remote.to_str().unwrap();
    crate::daemon::test_support::git(
        Path::new(checkout),
        &["remote", "add", "origin", remote_path],
    );
    crate::daemon::test_support::git(Path::new(checkout), &["fetch", "-q", "origin"]);
    let (talk, _talk) = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let converting = Converting {
        chat: talk,
        mode: ModeName::DEFAULT,
    };

    let workspace = manager
        .create_converted_workspace(converting, "origin/topic".into(), None, Default::default())
        .await
        .unwrap();

    assert_eq!(workspace.branch.as_deref(), Some("topic"));
}
