use crate::{
    config::modes::ModeName,
    daemon::{
        conversions::Converting,
        events::{EventItem, EventKind},
        manager::Manager,
        test_support::{chat, manager, repository},
    },
    model::Repository,
};
use shoal_protocol::space_names::SpaceSelector;

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// The kinds of the journal's events about the space `id`.
async fn kinds(manager: &Manager, id: &shoal_ids::space_ids::SpaceId) -> Vec<String> {
    (manager.space_events(None, 100).await.unwrap().into_iter())
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.space_id == *id => {
                Some(event.details.kind().to_string())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_worktree_that_cannot_be_made_gives_the_chat_back() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let talk = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let attacher = crate::daemon::workspace_files::Attacher::User;
    let done = (manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&talk.id),
        Default::default(),
        &attacher,
    ))
    .await
    .unwrap();
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let base = Some("no-such-ref".to_owned());
    let created = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "fix-login".into(),
            base,
            Default::default(),
        )
        .await;

    assert!(created.is_err());
    let restored = manager.spaces.chat_by_id(&talk.id).await.unwrap().unwrap();
    assert_eq!(
        (restored.number, restored.running, &restored.name),
        (talk.number, true, &talk.name)
    );
    assert!(manager.spaces.workspace_by_id(&talk.id).await.is_err());
    assert_eq!(
        manager.spaces.completion(&talk.id).await.unwrap(),
        Some(done)
    );
    let token = manager.spaces.chat_scope(&talk.id).await.unwrap();
    assert!(
        manager
            .coordination
            .caller(&*manager, &token)
            .await
            .unwrap()
            .is_some()
    );
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
async fn the_chats_open_threads_stay_with_the_space() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let talk = chat(&manager, "talk", "worker", Some(&repo), None).await;
    let selector = SpaceSelector::id(&talk.id);
    let asked = crate::daemon::test_support::ask(&manager, &talk.id, "Which database?").await;
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "fix-login".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();

    let open = shoal_protocol::threads::ThreadState::Open;
    let listed = manager
        .coordination
        .threads(&*manager, None, Some(&selector), open, false)
        .await
        .unwrap()
        .threads;
    let numbers: Vec<_> = listed.iter().map(|thread| thread.number).collect();
    assert_eq!(numbers, [asked.number]);
}

#[tokio::test]
async fn a_chat_stopped_before_its_conversion_records_stays_a_chat() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let talk = chat(&manager, "talk", "worker", Some(&repo), None).await;
    manager.agents.end_executions(&*manager, &talk.id).await;
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let refused = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "fix-login".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("stopped meanwhile"),
        "{refused:#}"
    );
    let chat = manager.spaces.chat_by_id(&talk.id).await.unwrap().unwrap();
    assert!(!chat.running);
}

#[tokio::test]
async fn a_conversion_during_the_repositorys_removal_waits_for_it() {
    let (root, manager) = manager().await;
    let manager = std::sync::Arc::new(manager);
    let repo = registered(root.path(), &manager).await;
    let first = chat(&manager, "first", "worker", Some(&repo), None).await;
    let second = chat(&manager, "second", "worker", Some(&repo), None).await;
    let converting = Converting {
        chat: first,
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };
    // Its agent waits to be handed over, which the removal does.
    manager
        .spaces
        .create_converted_workspace(
            &**manager,
            converting,
            "fix-login".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let git = manager.gates.lock_git(&repo.id).await;
    let removing = tokio::spawn({
        let (manager, id) = (manager.clone(), repo.id.clone());
        async move { manager.repositories.remove(&**manager, &id).await }
    });
    let converting = tokio::spawn({
        let manager = manager.clone();
        let converting = Converting {
            chat: second,
            mode: ModeName::DEFAULT,
            carrying: Default::default(),
        };
        async move {
            (manager.spaces.create_converted_workspace(
                &**manager,
                converting,
                "other".into(),
                None,
                Default::default(),
            ))
            .await
        }
    });
    // Whichever comes second waits for the other's hand-overs.
    let bound = std::time::Duration::from_secs(30);
    let waiting = crate::daemon::conversions::handovers::WAITING.notified();
    tokio::time::timeout(bound, waiting)
        .await
        .expect("the removal and the conversion both take the hand-overs first");
    drop(git);

    let removed = tokio::time::timeout(bound, removing).await;
    let converted = tokio::time::timeout(bound, converting).await;

    removed
        .expect("the removal waits on the conversion")
        .unwrap()
        .unwrap();
    converted
        .expect("the conversion waits on the removal")
        .unwrap()
        .ok();
    assert!(manager.repositories.repository(&repo.id).await.is_err());
}
