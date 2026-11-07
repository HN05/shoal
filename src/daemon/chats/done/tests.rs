use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::test_support::{ProcessGroup, manager, repository};

/// A running chat in a repository whose completions request no cleanup.
async fn chat(manager: &Manager, root: &std::path::Path, process: &ProcessGroup) -> Chat {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[cleanup.done]\nenabled = false\n".into()))
        .await
        .unwrap();
    manager
        .start_chat(
            repo.id.into(),
            "audit".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
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
async fn a_chats_summary_ends_its_notice_and_each_done_replaces_it() {
    let (root, manager) = manager().await;
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = chat(&manager, root.path(), &process).await;
    let completion = (manager.mark_chat_done(&chat.id, summarized("No leak"), &Attacher::User))
        .await
        .unwrap();
    assert_eq!(completion.summary.as_deref(), Some("No leak"));
    assert!(manager.done_notices().await[0].ends_with("; summary: No leak"));
    let recorded = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion, Some(completion));

    (manager.mark_chat_done(&chat.id, summarized("No leak"), &Attacher::User))
        .await
        .unwrap_err();
    (manager.mark_chat_done(&chat.id, summarized("A leak"), &Attacher::User))
        .await
        .unwrap();
    let notices = manager.done_notices().await;
    assert_eq!(notices.len(), 2);
    assert!(notices[1].ends_with("; summary: A leak"));

    (manager.mark_chat_done(&chat.id, Done::default(), &Attacher::User))
        .await
        .unwrap();
    let recorded = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion.unwrap().summary, None);

    (manager.mark_chat_done(&chat.id, summarized("Fixed"), &Attacher::User))
        .await
        .unwrap();
    let withdrawn = manager.undo_chat_done(&chat.id).await.unwrap();
    assert_eq!(withdrawn.summary.as_deref(), Some("Fixed"));
    let recorded = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion, None);
}

#[tokio::test]
async fn an_invalid_chat_summary_is_refused_before_anything_is_recorded() {
    let (root, manager) = manager().await;
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = chat(&manager, root.path(), &process).await;
    let report = root.path().join("report.md");
    std::fs::write(&report, "findings").unwrap();
    for summary in ["two\nlines".to_owned(), "x".repeat(513)] {
        let done = Done {
            results: vec![report.clone().into()],
            ..summarized(&summary)
        };
        (manager.mark_chat_done(&chat.id, done, &Attacher::User))
            .await
            .unwrap_err();
    }
    let recorded = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(recorded.completion, None);
    assert!(manager.done_notices().await.is_empty());
    let files = manager.attached_files(&SpaceSelector::id(&chat.id)).await;
    assert!(files.unwrap().is_empty());
}
