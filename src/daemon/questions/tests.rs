use super::*;
use crate::{
    daemon::scope::{self, Caller},
    model::{CleanupReason, Workspace},
    protocol::Method,
    removal::{BranchChoice, InspectionPolicy},
    test_support::{manager, repository},
};
use std::sync::Arc;

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "asker".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

fn ask(question: &str, choices: &[&str]) -> Ask {
    Ask {
        question: question.into(),
        recommendation: "SQLite".into(),
        reason: Some("No server to run".into()),
        choices: choices.iter().map(|&choice| choice.into()).collect(),
    }
}

#[tokio::test]
async fn asking_notifies_once_and_repeats_return_the_same_question() {
    let (_root, manager, workspace) = fixture().await;
    let first = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    let again = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    assert_eq!(again, first);
    assert!(first.is_open());
    assert_eq!(first.workspace, "asker");

    let notifications = manager.notifications(true, 50).await.unwrap();
    let asked: Vec<_> = notifications
        .iter()
        .filter(|n| n.kind == NotificationKind::QuestionAsked)
        .collect();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].message,
        format!(
            "question {} (answer with shoal answer {}): Which database? Recommended: SQLite",
            first.id, first.id
        )
    );

    let error = manager
        .ask_question(&workspace.name, ask("Which database?", &["SQLite"]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different details"));
    assert!(
        manager
            .ask_question(&workspace.name, ask("two\nlines", &[]))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn answers_resolve_choices_and_reach_the_agents_once() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &workspace.name,
            ask("Which database?", &["SQLite", "Postgres"]),
        )
        .await
        .unwrap();
    for invalid in ["3", "0", " "] {
        assert!(
            manager
                .answer_question(question.id, invalid.into())
                .await
                .is_err(),
            "{invalid:?}"
        );
    }
    let answered = manager
        .answer_question(question.id, "2".into())
        .await
        .unwrap();
    assert_eq!(answered.answer.as_deref(), Some("Postgres"));
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(answered.message_id, Some(messages[0].id));
    assert_eq!(
        messages[0].message,
        format!(
            "Answer to question {} (Which database?): Postgres",
            question.id
        )
    );
    let error = manager
        .answer_question(question.id, "1".into())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already answered"));

    let open = manager.questions(None, true).await.unwrap();
    assert!(open.questions.is_empty());
    let all = manager
        .questions(Some(&workspace.name), false)
        .await
        .unwrap();
    assert_eq!(all.questions, [answered]);
}

#[tokio::test]
async fn free_text_answers_may_span_lines() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    let answered = manager
        .answer_question(question.id, "Postgres,\nsince staging has it".into())
        .await
        .unwrap();
    assert_eq!(
        answered.answer.as_deref(),
        Some("Postgres,\nsince staging has it")
    );
}

#[tokio::test]
async fn asking_again_returns_an_undelivered_answer_then_asks_anew() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    let answered = manager
        .answer_question(question.id, "Postgres".into())
        .await
        .unwrap();
    let again = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    assert_eq!(again, answered);

    let message = answered.message_id.unwrap();
    manager
        .mark_agent_messages_delivered(&workspace.name, vec![message])
        .await
        .unwrap();
    let delivered = manager.questions(None, false).await.unwrap();
    assert_eq!(delivered.questions[0].message_id, None);
    let new = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    assert_ne!(new.id, question.id);
    assert!(new.is_open());
}

#[tokio::test]
async fn a_wait_returns_the_answer() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    let waiter = tokio::spawn({
        let manager = manager.clone();
        let name = workspace.name.clone();
        async move { manager.await_answer(&name, question.id).await }
    });
    manager
        .answer_question(question.id, "Postgres".into())
        .await
        .unwrap();
    let answered = waiter.await.unwrap().unwrap();
    assert_eq!(answered.answer.as_deref(), Some("Postgres"));

    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    let error = manager
        .await_answer(&other.name, question.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("has no question"));
}

#[tokio::test]
async fn open_questions_keep_the_workspace_and_removal_deletes_them() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Question
    );
    manager
        .answer_question(question.id, "Postgres".into())
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Idle { minutes: 10 }
    );

    manager
        .ask_question(&workspace.name, ask("Which cache?", &[]))
        .await
        .unwrap();
    manager
        .remove_workspace(
            &workspace.id,
            BranchChoice::KeepBranch,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(
        manager
            .questions(None, false)
            .await
            .unwrap()
            .questions
            .is_empty()
    );
}

#[tokio::test]
async fn workspace_processes_ask_and_wait_but_only_the_user_answers() {
    let (_root, manager, workspace) = fixture().await;
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
            Method::AskQuestion {
                workspace: target.clone(),
                ask: ask("Which database?", &[]),
            },
            Method::AwaitAnswer {
                workspace: target.clone(),
                id: 1,
            },
            Method::ListQuestions {
                workspace: Some(target.clone()),
                all: false,
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
    let mut listing = Method::ListQuestions {
        workspace: None,
        all: true,
    };
    scope::authorize(&manager, Some("scope"), &mut listing)
        .await
        .unwrap();
    assert!(matches!(
        listing,
        Method::ListQuestions { workspace: Some(owner), .. } if owner == workspace.id
    ));
    for mut method in [
        Method::AnswerQuestion {
            id: 1,
            answer: "yes".into(),
        },
        Method::ShowQuestion { id: 1 },
    ] {
        assert!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_err()
        );
    }
}

/// The largest question `ask` accepts, with the largest answer.
fn maximal(n: usize) -> Ask {
    let line = |label: &str| format!("{label} {n} {}", "\"".repeat(400));
    Ask {
        question: line("question"),
        recommendation: line("recommendation"),
        reason: Some(line("reason")),
        choices: (0..MAX_CHOICES)
            .map(|i| line(&format!("choice {i}")))
            .collect(),
    }
}

#[tokio::test]
async fn listings_and_unread_answers_fit_one_response() {
    let (_root, manager, workspace) = fixture().await;
    let answer = "\"\n".repeat(MAX_ANSWER_LEN / 2);
    let mut unread = 0;
    let full = loop {
        let question = manager
            .ask_question(&workspace.name, maximal(unread))
            .await
            .unwrap();
        match manager.answer_question(question.id, answer.clone()).await {
            Ok(_) => unread += 1,
            Err(error) => break error,
        }
    };
    assert!(full.to_string().contains("would exceed"), "{full:#}");
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    assert_eq!(messages.len(), unread);
    let body = crate::protocol::Body::AgentMessages(messages);
    assert!(serde_json::to_vec(&body).unwrap().len() < crate::protocol::MAX_FRAME);

    let listing = manager.questions(None, false).await.unwrap();
    assert!(listing.omitted > 0);
    assert_eq!(listing.omitted + listing.questions.len(), unread + 1);
    let newest = listing.questions.last().unwrap();
    assert!(newest.is_open());
    let body = crate::protocol::Body::Questions(listing);
    assert!(serde_json::to_vec(&body).unwrap().len() < crate::protocol::MAX_FRAME);
}

#[tokio::test]
async fn notifications_of_maximal_questions_fit_one_response() {
    let (_root, manager, workspace) = fixture().await;
    let mut first = None;
    for n in 0..50 {
        let question = manager
            .ask_question(&workspace.name, maximal(n))
            .await
            .unwrap();
        first.get_or_insert(question);
    }
    let notifications = manager.notifications(true, 50).await.unwrap();
    assert_eq!(notifications.len(), 50);
    let first = first.unwrap();
    let message = &notifications[0].message;
    assert!(message.len() <= crate::validate::MAX_MESSAGE_LEN);
    assert!(message.starts_with(&format!(
        "question {} (answer with shoal answer {}): question 0 ",
        first.id, first.id
    )));
    assert!(message.ends_with('…'));
    let body = crate::protocol::Body::Notifications(notifications);
    assert!(serde_json::to_vec(&body).unwrap().len() < crate::protocol::MAX_FRAME);

    assert_eq!(manager.question(first.id).await.unwrap(), first);
    assert!(manager.question(first.id + 100).await.is_err());
}
