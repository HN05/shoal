use super::*;
use crate::daemon::{
    test_support::{commit, install_test_script, manager, repository},
    workspace_files::Attacher,
};
use std::{fs, sync::Arc};

#[tokio::test]
async fn completion_without_a_hook_allows_concurrent_resource_hooks() {
    let (_root, manager, workspace) = fixture().await;
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
        .mark_done(
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
        .mark_done(
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
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .cleanup
    );
    drop(resources);
    assert!(
        manager
            .mark_done(
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
async fn completion_persists_and_cleanup_overrides_the_repository_default() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(workspace.repository()).await.unwrap();
    assert!(
        manager
            .mark_done(
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
            .mark_done(
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
            .mark_done(
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
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(!kept.cleanup);
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let completion = reopened
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .completion
        .unwrap();
    assert!(!completion.cleanup);
    assert_eq!(completion.head, kept.head);
    assert!(
        reopened
            .notifications(true, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == NotificationKind::SpaceDone)
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
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap_err();
    // Repeating it closes a question asked since.
    manager
        .ask_question(
            &SpaceSelector::id(&workspace.id),
            crate::daemon::questions::Ask {
                question: "Anything more?".into(),
                recommendation: "No".into(),
                reason: None,
                choices: vec![],
            },
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(!manager.has_open_questions(&workspace.id).await.unwrap());
    // Another cleanup choice or HEAD replaces the recorded completion.
    assert!(
        manager
            .mark_done(
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
        .mark_done(
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
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .head,
        newer.head
    );
}

#[tokio::test]
async fn repeating_a_completion_reruns_a_configured_hook() {
    let (root, manager, workspace) = fixture().await;
    let runs = root.path().join("runs");
    let hook = root.path().join("post-done");
    install_test_script(
        &hook,
        &format!("#!/bin/sh\nprintf x >> '{}'\nexit 1\n", runs.display()),
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
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // After the hook failed, the same completion runs it again.
    manager
        .mark_done(
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
    let (_root, manager, workspace) = fixture().await;
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
            .undo_done(&SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
    assert!(manager.completion(&workspace.id).await.is_err());
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
        .mark_done(
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
        .mark_done(&selector, same, &Attacher::User)
        .await
        .unwrap_err();
    manager
        .mark_done(
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
        .mark_done(&selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
    let recorded = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert_eq!(recorded.summary, None);
    assert!(!manager.done_notices().await[2].contains("summary"));

    manager
        .mark_done(&selector, summarized("Fixed"), &Attacher::User)
        .await
        .unwrap();
    assert_eq!(
        manager
            .undo_done(&selector)
            .await
            .unwrap()
            .summary
            .as_deref(),
        Some("Fixed")
    );
    assert!(manager.completion(&workspace.id).await.unwrap().is_none());
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
            .mark_done(&SpaceSelector::id(&workspace.id), done, &Attacher::User)
            .await
            .unwrap_err();
    }
    assert!(manager.completion(&workspace.id).await.unwrap().is_none());
    assert!(manager.done_notices().await.is_empty());
    let files = manager
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(files.is_empty());
}

#[test]
fn a_completion_recorded_without_a_summary_reads() {
    let completion: Completion =
        serde_json::from_str(r#"{"head":"abc","cleanup":true,"integrated":false,"error":null}"#)
            .unwrap();
    assert_eq!(completion.summary, None);
}

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}
