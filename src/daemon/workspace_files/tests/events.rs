use super::{Attacher, FileSource, chat, workspace};
use crate::client::assignments::Done;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    client::events::{EventItem, FileEventDetails, FileEventKind},
    daemon::{test_support::manager, workspace::Manager},
};

/// The journal's file events, oldest first.
async fn file_events(manager: &Manager) -> Vec<FileEventDetails> {
    manager
        .workspace_events(None, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::File(event) => Some(event.details),
            _ => None,
        })
        .collect()
}

/// The kind, file name and source of each event.
fn summary(events: &[FileEventDetails]) -> Vec<(FileEventKind, &str, FileSource)> {
    events
        .iter()
        .map(|event| (event.kind, event.file.name.as_str(), event.file.source))
        .collect()
}

async fn remove(manager: &Manager, id: &SpaceId) {
    manager
        .remove_workspace(
            &SpaceSelector::id(id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn every_route_records_one_added_event_per_file() {
    use FileEventKind::Added;
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (log, shot) = (temp.path().join("crash.log"), temp.path().join("shot.png"));
    std::fs::write(&log, "panic").unwrap();
    std::fs::write(&shot, "image").unwrap();
    // Sending the same files again keeps their records, so adds nothing.
    for _ in 0..2 {
        manager
            .send_agent_message(
                &SpaceSelector::from(&workspace.name),
                "See".into(),
                vec![log.clone().into(), shot.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![crate::daemon::workspace_files::NewFile {
                path: log.clone(),
                title: Some("Crash log".into()),
            }],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![shot.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();

    let events = file_events(&manager).await;
    assert_eq!(
        summary(&events),
        [
            (Added, "crash.log", FileSource::Message),
            (Added, "shot.png", FileSource::Message),
            (Added, "crash.log", FileSource::Added),
            (Added, "shot.png", FileSource::Result),
        ]
    );
    let listed = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let added = &events[2];
    assert_eq!(added.workspace_id.as_deref(), Some(workspace.id.as_str()));
    assert_eq!(added.chat_id, None);
    assert_eq!(added.repository_id.as_ref(), Some(&workspace.repository_id));
    // The event's file is the record as `shoal files` lists it.
    assert_eq!(added.file, listed[2]);
    assert_eq!(added.file.title.as_deref(), Some("Crash log"));
    assert_eq!(added.file.attached_by, Some(Attacher::User));

    let item = manager.workspace_events(None, 1000).await.unwrap();
    let line = item
        .iter()
        .find(|item| matches!(item, EventItem::File(_)))
        .map(serde_json::to_value)
        .unwrap()
        .unwrap();
    assert_eq!(line["type"], "file");
    assert_eq!(line["kind"], "file_added");
    assert_eq!(line["file"]["workspace"], workspace.name.as_str());
}

#[tokio::test]
async fn every_deletion_records_one_removed_event_per_file() {
    use FileEventKind::Removed;
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&workspace.id),
            "audit".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let file = |name: &str| {
        let path = temp.path().join(name);
        std::fs::write(&path, name).unwrap();
        crate::daemon::workspace_files::NewFile::from(path)
    };
    let attach = |space: String, name: &str| {
        let file = file(name);
        let manager = &manager;
        async move {
            manager
                .add_attached_files(&SpaceSelector::parse(&space), vec![file], &Attacher::User)
                .await
                .unwrap();
        }
    };
    // `shoal files rm --all`, which also takes kept results, so goes first.
    attach(workspace.name.to_string(), "idle.md").await;
    manager.remove_idle_files().await.unwrap();
    attach(workspace.name.to_string(), "deleted.md").await;
    attach(child.name.to_string(), "removed.md").await;
    for result in ["expired.md", "low-disk.md"] {
        let audit = manager
            .split_workspace(
                &SpaceSelector::id(&workspace.id),
                result.trim_end_matches(".md").into(),
                None,
                Default::default(),
            )
            .await
            .unwrap();
        manager
            .mark_done(
                &SpaceSelector::id(&audit.id),
                Done {
                    results: vec![file(result)],
                    ..Done::default()
                },
                &Attacher::User,
            )
            .await
            .unwrap();
        remove(&manager, &audit.id).await;
    }

    // `shoal files rm`
    manager
        .remove_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec!["deleted.md".into()],
            vec![],
        )
        .await
        .unwrap();
    // Removal deletes all but the results.
    manager
        .mark_done(
            &SpaceSelector::id(&child.id),
            Done {
                results: vec![file("kept.md")],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    remove(&manager, &child.id).await;
    // The retention sweep deletes the results whose removal record is gone.
    let expired = manager
        .kept_results()
        .await
        .unwrap()
        .into_iter()
        .find(|space| space.name == "expired")
        .unwrap();
    let id = expired.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM removed_workspaces WHERE workspace_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    // Disk cleanup
    let low = manager
        .kept_results()
        .await
        .unwrap()
        .into_iter()
        .find(|space| space.name == "low-disk")
        .unwrap();
    assert!(manager.delete_results_in(&low, |_| true).await.unwrap());
    // The repository's removal deletes what is left.
    remove(&manager, &workspace.id).await;
    manager
        .remove_repository(&workspace.repository_id)
        .await
        .unwrap();

    let events = file_events(&manager).await;
    let removed: Vec<_> = events
        .into_iter()
        .filter(|event| event.kind == Removed)
        .collect();
    assert_eq!(
        summary(&removed),
        [
            (Removed, "idle.md", FileSource::Added),
            (Removed, "deleted.md", FileSource::Added),
            (Removed, "removed.md", FileSource::Added),
            (Removed, "expired.md", FileSource::Result),
            (Removed, "low-disk.md", FileSource::Result),
            (Removed, "kept.md", FileSource::Result),
        ]
    );
    // A removed workspace's events keep its identity.
    let gone = &removed[3];
    assert_eq!(gone.workspace_id.as_deref(), Some(expired.id.as_str()));
    assert_eq!(gone.file.workspace.as_deref(), Some("expired"));
    assert_eq!(gone.repository_id.as_ref(), Some(&workspace.repository_id));
}

#[tokio::test]
async fn a_chats_file_events_name_the_chat() {
    use FileEventKind::{Added, Removed};
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let chat = chat(&manager, &workspace, "planning", &process).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    manager
        .add_attached_files(
            &SpaceSelector::from(&chat.name),
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute("UPDATE ended_chats SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();

    let events = file_events(&manager).await;
    assert_eq!(
        summary(&events),
        [
            (Added, "report.md", FileSource::Added),
            (Removed, "report.md", FileSource::Added),
        ]
    );
    for event in &events {
        assert_eq!(event.chat_id.as_deref(), Some(chat.id.as_str()));
        assert_eq!(event.workspace_id, None);
        assert_eq!(event.file.chat.as_deref(), Some("planning"));
    }
}

#[tokio::test]
async fn the_file_events_of_a_chat_without_a_repository_name_none() {
    use FileEventKind::{Added, Removed};
    let (temp, manager) = manager().await;
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = crate::daemon::chats::NewChat {
        narrowing: Default::default(),
        name: "master".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: None,
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    manager
        .add_attached_files(
            &SpaceSelector::from(&chat.name),
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute("UPDATE ended_chats SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();

    let events = file_events(&manager).await;
    assert_eq!(
        summary(&events),
        [
            (Added, "report.md", FileSource::Added),
            (Removed, "report.md", FileSource::Added),
        ]
    );
    for event in &events {
        assert_eq!(event.chat_id.as_deref(), Some(chat.id.as_str()));
        assert_eq!(event.repository_id, None);
    }
}
