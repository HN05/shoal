use crate::stand_in::Fixture;
use crate::{
    config::modes::ModeName,
    conversions::{Converting, Pending},
    events::{EventItem, EventKind},
};
use shoal_core::test_support::{git, repository};
use shoal_protocol::{progress::ProgressChange, space_names::SpaceSelector};
use shoal_refs::BranchName;
use std::path::Path;

/// The kinds of the journal's events about the space `id`.
async fn kinds(manager: &Fixture, id: &shoal_ids::space_ids::SpaceId) -> Vec<String> {
    (manager.space_events(None, 100).await.unwrap().into_iter())
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.space_id == *id => {
                Some(event.details.kind.to_string())
            }
            _ => None,
        })
        .collect()
}
#[tokio::test]
async fn a_chat_becomes_a_workspace_under_its_id_number_name_and_parent() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let lead = manager.chat("lead", "chat", Some(&repo), None).await;
    let talk = manager
        .chat("talk", "worker", Some(&repo), Some(&lead.id))
        .await;
    manager.run_agent(&talk.id).await;
    let status = ProgressChange::Status(Some("Reproduced".into()));
    (manager
        .spaces
        .set_progress(&*manager, &SpaceSelector::id(&talk.id), status))
    .await
    .unwrap();
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let workspace = manager
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

    assert_eq!(
        (&workspace.id, workspace.number, &workspace.name),
        (&talk.id, talk.number, &talk.name)
    );
    assert_eq!(
        workspace.branch().map(BranchName::as_str),
        Some("fix-login")
    );
    assert!(
        workspace.path.ends_with("fix-login"),
        "{:?}",
        workspace.path
    );
    assert_eq!(workspace.mode, Some(ModeName::DEFAULT));
    assert_eq!(workspace.parent.map(|parent| parent.id), Some(lead.id));
    let progress = workspace.progress.and_then(|progress| progress.status);
    assert_eq!(progress.as_deref(), Some("Reproduced"));
    assert!(manager.spaces.chat_by_id(&talk.id).await.unwrap().is_none());
    let id = talk.id.clone();
    let pending = (manager.store)
        .run(move |db| crate::conversions::pending(db, &id))
        .await
        .unwrap();
    let Some(Pending::Workspace { execution, .. }) = pending else {
        panic!("expected the chat's restart to wait: {pending:?}");
    };
    // The chat's agent hands its session over under the execution it runs as.
    let executions = manager.spaces.workspace_executions(&talk.id).await.unwrap();
    let agent = (executions.iter()).find(|recorded| recorded.agent.is_some());
    assert_eq!(agent.map(|recorded| &recorded.id), Some(&execution));
    // The space stays: its conversion is neither a removal nor a creation.
    let kinds = kinds(&manager, &talk.id).await;
    let converted = (kinds.iter())
        .position(|kind| *kind == EventKind::Converted.to_string())
        .unwrap_or_else(|| panic!("no conversion: {kinds:?}"));
    for gone in [
        EventKind::Created.to_string(),
        EventKind::Removed.to_string(),
    ] {
        assert!(!kinds[converted..].contains(&gone), "{kinds:?}");
    }
}

#[tokio::test]
async fn a_branch_another_workspace_holds_is_refused_before_anything_changes() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let holder = manager
        .create_workspace(&repo.id, "fix-login".into(), None, None, None)
        .await
        .unwrap();
    let talk = manager.chat("talk", "worker", Some(&repo), None).await;
    manager.run_agent(&talk.id).await;
    let converting = Converting {
        chat: talk.clone(),
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let error = manager
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
        format!("{error:#}").contains(holder.name.as_str()),
        "{error:#}"
    );
    assert!(manager.spaces.chat_by_id(&talk.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_remote_branch_named_with_its_remote_opens_as_that_branch() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let remote = repository(root.path(), "remote");
    git(&remote, &["branch", "topic"]);
    let repo = manager.repository("repo").await;
    let checkout = repo.path.to_str().unwrap();
    let remote_path = remote.to_str().unwrap();
    git(
        Path::new(checkout),
        &["remote", "add", "origin", remote_path],
    );
    git(Path::new(checkout), &["fetch", "-q", "origin"]);
    let talk = manager.chat("talk", "worker", Some(&repo), None).await;
    manager.run_agent(&talk.id).await;
    let converting = Converting {
        chat: talk,
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let workspace = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "origin/topic".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();

    assert_eq!(workspace.branch().map(BranchName::as_str), Some("topic"));
}

#[tokio::test]
async fn a_branch_only_the_remote_has_opens_without_a_fetch() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let remote = repository(root.path(), "remote");
    git(&remote, &["branch", "topic"]);
    let repo = manager.repository("repo").await;
    let remote_path = remote.to_str().unwrap();
    git(&repo.path, &["remote", "add", "origin", remote_path]);
    let talk = manager.chat("talk", "worker", Some(&repo), None).await;
    manager.run_agent(&talk.id).await;
    let converting = Converting {
        chat: talk,
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let workspace = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "topic".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();

    let remote_head = git(&remote, &["rev-parse", "topic"]);
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    assert_eq!(head, remote_head);
}

#[tokio::test]
async fn a_new_branch_whose_namespace_a_branch_blocks_is_allocated_as_any() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    git(&repo.path, &["branch", "feature"]);
    let talk = manager.chat("talk", "worker", Some(&repo), None).await;
    manager.run_agent(&talk.id).await;
    let converting = Converting {
        chat: talk,
        mode: ModeName::DEFAULT,
        carrying: Default::default(),
    };

    let workspace = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "feature/login".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        workspace.branch().map(BranchName::as_str),
        Some("feature-2/login")
    );
}

#[tokio::test]
async fn a_mode_the_branch_does_not_define_gives_the_chat_back() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let checkout = repo.path.clone();
    git(&checkout, &["branch", "bare"]);
    std::fs::write(
        checkout.join(".shoal.toml"),
        "[modes.extra]\nkind = \"workspace\"\n",
    )
    .unwrap();
    git(&checkout, &["add", ".shoal.toml"]);
    git(&checkout, &["commit", "-qm", "Add a mode"]);
    let talk = manager.chat("talk", "worker", Some(&repo), None).await;
    manager.run_agent(&talk.id).await;
    let converting = Converting {
        chat: talk.clone(),
        mode: "extra".into(),
        carrying: Default::default(),
    };

    let refused = manager
        .spaces
        .create_converted_workspace(
            &*manager,
            converting,
            "bare".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap_err();

    assert!(
        format!("{refused:#}").contains("no mode extra"),
        "{refused:#}"
    );
    assert!(manager.spaces.chat_by_id(&talk.id).await.unwrap().is_some());
    // It was never ready for commands or leases to use.
    let kinds = kinds(&manager, &talk.id).await;
    assert!(!kinds.contains(&EventKind::Ready.to_string()), "{kinds:?}");
}
