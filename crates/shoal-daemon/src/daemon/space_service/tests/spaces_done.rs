use crate::daemon::manager::Manager;
use crate::daemon::notifications::AlertKind;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::{
    test_support::{commit, install_test_script, manager, repository},
    workspace_files::Attacher,
};
use crate::model::{Chat, Workspace};
use std::{fs, sync::Arc};

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

/// A running chat in a repository whose completions request no cleanup.
async fn chat_in_repository(manager: &std::sync::Arc<Manager>, root: &std::path::Path) -> Chat {
    let checkout = repository(root, "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[cleanup.done]\nenabled = false\n".into()))
        .await
        .unwrap();
    manager
        .start_chat(repo.id.into(), "audit".into(), "claude".into(), None)
        .await
        .unwrap()
}

fn summarized(summary: &str) -> Done {
    Done {
        summary: Some(summary.into()),
        ..Done::default()
    }
}
#[tokio::test]
async fn completion_persists_and_cleanup_overrides_the_repository_default() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager
        .repositories
        .repository(workspace.repository())
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done::default(),
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    manager
        .write_local_config(&repo.id, Some("[cleanup.done]\nenabled=false\n".into()))
        .await
        .unwrap();
    assert!(
        !manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done::default(),
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done {
                    cleanup: true,
                    ..Done::default()
                },
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    let kept = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(!kept.cleanup);
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let completion = reopened
        .spaces
        .inspect_workspace(&*reopened, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .completion
        .unwrap();
    assert!(!completion.cleanup);
    assert_eq!(completion.head, kept.head);
    assert!(
        reopened
            .notifications
            .list(true, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == AlertKind::SpaceDone)
    );
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn repeating_an_unchanged_completion_is_refused() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap_err();
    // Repeating it closes a thread opened since.
    crate::daemon::test_support::ask(&manager, &workspace.id, "Anything more?").await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(
        !manager
            .coordination
            .has_waiting_threads(&workspace.id)
            .await
            .unwrap()
    );
    // Another cleanup choice or HEAD replaces the recorded completion.
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done {
                    cleanup: true,
                    ..Done::default()
                },
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
    fs::write(workspace.path.join("more-work"), "more").unwrap();
    commit(&workspace.path, "more-work");
    let newer = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .head,
        newer.head
    );
}

#[tokio::test]
async fn a_summary_ends_the_notice_and_each_done_replaces_it() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let summarized = |summary: &str| Done {
        summary: Some(summary.into()),
        ..Done::default()
    };
    let completion = manager
        .spaces
        .mark_space_done(
            &*manager,
            &selector,
            summarized("No leak; the cache is freed"),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        completion.summary.as_deref(),
        Some("No leak; the cache is freed")
    );
    let notices = manager.done_notices().await;
    assert!(notices[0].ends_with("; summary: No leak; the cache is freed"));

    // The same summary at the same HEAD changes nothing; another one does.
    let same = summarized("No leak; the cache is freed");
    manager
        .spaces
        .mark_space_done(&*manager, &selector, same, &Attacher::User)
        .await
        .unwrap_err();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &selector,
            summarized("A leak in the cache"),
            &Attacher::User,
        )
        .await
        .unwrap();
    let notices = manager.done_notices().await;
    assert_eq!(notices.len(), 2);
    assert!(notices[1].ends_with("; summary: A leak in the cache"));

    // A done without one records none.
    manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
    let recorded = manager
        .spaces
        .completion(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded.summary, None);
    assert!(!manager.done_notices().await[2].contains("summary"));

    manager
        .spaces
        .mark_space_done(&*manager, &selector, summarized("Fixed"), &Attacher::User)
        .await
        .unwrap();
    assert_eq!(
        manager
            .spaces
            .undo_space_done(&*manager, &selector)
            .await
            .unwrap()
            .summary
            .as_deref(),
        Some("Fixed")
    );
    assert!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn an_invalid_summary_is_refused_before_anything_is_recorded() {
    let (_root, manager, workspace) = fixture().await;
    let report = workspace.path.join("report.md");
    fs::write(&report, "findings").unwrap();
    for summary in [
        "two\nlines".to_owned(),
        "x".repeat(crate::validate::MAX_MESSAGE_LEN + 1),
    ] {
        let done = Done {
            summary: Some(summary),
            results: vec![report.clone().into()],
            ..Done::default()
        };
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                done,
                &Attacher::User,
            )
            .await
            .unwrap_err();
    }
    assert!(
        manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(manager.done_notices().await.is_empty());
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(files.is_empty());
}

#[tokio::test]
async fn a_chats_summary_ends_its_notice_and_each_done_replaces_it() {
    let (root, manager) = manager().await;
    let chat = chat_in_repository(&manager, root.path()).await;
    let completion = (manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&chat.id),
        summarized("No leak"),
        &Attacher::User,
    ))
    .await
    .unwrap();
    assert_eq!(completion.summary.as_deref(), Some("No leak"));
    assert!(manager.done_notices().await[0].ends_with("; summary: No leak"));
    let recorded = manager.spaces.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion, Some(completion));

    (manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&chat.id),
        summarized("No leak"),
        &Attacher::User,
    ))
    .await
    .unwrap_err();
    (manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&chat.id),
        summarized("A leak"),
        &Attacher::User,
    ))
    .await
    .unwrap();
    let notices = manager.done_notices().await;
    assert_eq!(notices.len(), 2);
    assert!(notices[1].ends_with("; summary: A leak"));

    (manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&chat.id),
        Done::default(),
        &Attacher::User,
    ))
    .await
    .unwrap();
    let recorded = manager.spaces.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.part.completion.unwrap().summary, None);

    (manager.spaces.mark_space_done(
        &*manager,
        &SpaceSelector::id(&chat.id),
        summarized("Fixed"),
        &Attacher::User,
    ))
    .await
    .unwrap();
    let withdrawn = manager
        .spaces
        .undo_space_done(&*manager, &SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    assert_eq!(withdrawn.summary.as_deref(), Some("Fixed"));
    let recorded = manager.spaces.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion, None);
}

#[tokio::test]
async fn an_invalid_chat_summary_is_refused_before_anything_is_recorded() {
    let (root, manager) = manager().await;
    let chat = chat_in_repository(&manager, root.path()).await;
    let report = root.path().join("report.md");
    std::fs::write(&report, "findings").unwrap();
    for summary in [
        "two\nlines".to_owned(),
        "x".repeat(crate::validate::MAX_MESSAGE_LEN + 1),
    ] {
        let done = Done {
            results: vec![report.clone().into()],
            ..summarized(&summary)
        };
        (manager.spaces.mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            done,
            &Attacher::User,
        ))
        .await
        .unwrap_err();
    }
    let recorded = manager.spaces.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion, None);
    assert!(manager.done_notices().await.is_empty());
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&chat.id))
        .await;
    assert!(files.unwrap().is_empty());
}

#[tokio::test]
async fn a_chats_post_done_hook_runs_where_it_works() {
    let (root, manager) = manager().await;
    let chat = chat_in_repository(&manager, root.path()).await;
    let ran_in = root.path().join("ran-in");
    let hook = root.path().join("post-done");
    let script = format!("#!/bin/sh\npwd -P > '{}'\n", ran_in.display());
    install_test_script(&hook, &script);
    let config = format!(
        "hooks.post_done = '{}'\n[cleanup.done]\nenabled = false\n",
        hook.display()
    );
    let repository = chat.repository_id.clone().unwrap();
    (manager.write_local_config(&repository, Some(config)))
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);
    (manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User))
    .await
    .unwrap();
    let workdir = fs::canonicalize(chat.workdir.path()).unwrap();
    assert_eq!(
        fs::read_to_string(&ran_in).unwrap().trim_end(),
        workdir.to_str().unwrap()
    );
}

#[tokio::test]
async fn a_chat_in_a_worktree_records_the_worktrees_head() {
    let (_root, manager, workspace) = fixture().await;
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "reader".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    fs::write(workspace.path.join("more-work"), "more").unwrap();
    commit(&workspace.path, "more-work");
    let selector = SpaceSelector::id(&chat.id);
    let completion =
        (manager
            .spaces
            .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User))
        .await
        .unwrap();
    let (head, _) = crate::git::head_and_branch(&workspace.path).await.unwrap();
    assert_eq!(completion.head, head);
}
