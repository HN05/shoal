use crate::gates::GuardMode;
use crate::model::Completion;
use crate::model::HandedBack;
use crate::spaces::done::*;
use crate::stand_in::Fixture;
use shoal_core::test_support::{commit, install_test_script, repository};
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;

use crate::model::Workspace;
use crate::workspace_files::Attacher;
use std::fs;

async fn fixture() -> (Fixture, Workspace) {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (manager, workspace)
}

#[tokio::test]
async fn completion_without_a_hook_allows_concurrent_resource_hooks() {
    let (manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    let resources = manager
        .gates
        .resource_guard(&workspace.id, GuardMode::Exclusive)
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
        .write_local_config(
            workspace.repository(),
            Some("hooks.post_done = '/usr/bin/true'\n[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager
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
        .unwrap_err();
    assert!(
        !manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .cleanup
    );
    drop(resources);
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
}

#[tokio::test]
async fn repeating_a_completion_reruns_a_configured_hook() {
    let (manager, workspace) = fixture().await;
    let root = &manager.root;
    let runs = root.path().join("runs");
    let hook = root.path().join("post-done");
    install_test_script(
        &hook,
        format!("#!/bin/sh\nprintf x >> '{}'\nexit 1\n", runs.display()),
    );
    manager
        .write_local_config(
            workspace.repository(),
            Some(format!(
                "hooks.post_done = '{}'\n[cleanup.done]\nenabled = false\n",
                hook.display()
            )),
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
    // After the hook failed, the same completion runs it again.
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
    assert_eq!(fs::read_to_string(&runs).unwrap(), "xx");
}

#[tokio::test]
async fn undone_keeps_an_unreadable_completion() {
    let (manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_completion(space_id,record,cause) VALUES (?1,'{','manual')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
    assert!(manager.spaces.completion(&workspace.id).await.is_err());
}

#[test]
fn a_notice_names_the_first_results_and_counts_the_rest() {
    let results: Vec<_> = ["report.md", "repro.sh", "trace.log", "a.png", "b.png"]
        .into_iter()
        .zip(1..)
        .map(|(name, id)| HandedBack {
            id,
            name: name.into(),
        })
        .collect();
    assert_eq!(
        results_preview(&results),
        "report.md, repro.sh, trace.log and 2 more"
    );
    assert_eq!(results_preview(&results[..2]), "report.md, repro.sh");
}

#[test]
fn a_notice_selects_the_space_whose_results_it_names_by_number() {
    let completion = Completion {
        head: "head".into(),
        cleanup: false,
        integrated: false,
        results: vec![HandedBack {
            id: 1,
            name: "report.md".into(),
        }],
        summary: None,
        error: None,
    };
    let notice = done_notice("7", &completion, &[]);
    assert!(
        notice.contains("results in shoal files 7: report.md"),
        "{notice}"
    );
}

#[test]
fn a_completion_recorded_without_a_summary_reads() {
    let completion: Completion =
        serde_json::from_str(r#"{"head":"abc","cleanup":true,"integrated":false,"error":null}"#)
            .unwrap();
    assert_eq!(completion.summary, None);
}

/// A chat in a new repository whose completions request no cleanup.
async fn chat_in_repository(manager: &Fixture) -> crate::model::Chat {
    let repo = manager.repository("repo").await;
    manager
        .write_local_config(&repo.id, Some("[cleanup.done]\nenabled = false\n".into()))
        .await
        .unwrap();
    manager.chat("audit", "chat", Some(&repo), None).await
}

fn summarized(summary: &str) -> Done {
    Done {
        summary: Some(summary.into()),
        ..Done::default()
    }
}

#[tokio::test]
async fn a_summary_ends_the_notice_and_each_done_replaces_it() {
    let (manager, workspace) = fixture().await;
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
    let (manager, workspace) = fixture().await;
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
    let manager = Fixture::new().await;
    let chat = chat_in_repository(&manager).await;
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
    let manager = Fixture::new().await;
    let root = &manager.root;
    let chat = chat_in_repository(&manager).await;
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
    let manager = Fixture::new().await;
    let root = &manager.root;
    let chat = chat_in_repository(&manager).await;
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
    let (manager, workspace) = fixture().await;
    let chat = manager
        .chat_in(&SpaceSelector::id(&workspace.id), "reader")
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
