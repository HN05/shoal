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
                .answer_question(question.id, invalid.into(), None)
                .await
                .is_err(),
            "{invalid:?}"
        );
    }
    let answered = manager
        .answer_question(question.id, "2".into(), None)
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
        .answer_question(question.id, "1".into(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("already answered"));

    let open = manager.questions(Some(&workspace.name)).await.unwrap();
    assert!(open.questions.is_empty());
    assert_eq!(manager.question(question.id).await.unwrap(), answered);
}

#[tokio::test]
async fn free_text_answers_may_span_lines() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    let answered = manager
        .answer_question(question.id, "Postgres,\nsince staging has it".into(), None)
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
        .answer_question(question.id, "Postgres".into(), None)
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
    // Delivery deletes the question and frees its number.
    assert!(manager.question(question.id).await.is_err());
    let new = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap();
    assert_eq!(new.id, question.id);
    assert!(new.is_open());
}

#[tokio::test]
async fn questions_take_the_smallest_free_number_across_workspaces() {
    let (_root, manager, workspace) = fixture().await;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    let mut numbers = Vec::new();
    for (asker, text) in [
        (&workspace, "Which database?"),
        (&other, "Which cache?"),
        (&workspace, "Which queue?"),
    ] {
        let question = manager
            .ask_question(&asker.name, ask(text, &[]))
            .await
            .unwrap();
        numbers.push(question.id);
    }
    assert_eq!(numbers, [1, 2, 3]);

    // An answer keeps its number until the agent receives it.
    let answered = manager
        .answer_question(2, "Redis".into(), None)
        .await
        .unwrap();
    let next = manager
        .ask_question(&workspace.name, ask("Which logger?", &[]))
        .await
        .unwrap();
    assert_eq!(next.id, 4);
    manager
        .mark_agent_messages_delivered(&other.name, vec![answered.message_id.unwrap()])
        .await
        .unwrap();
    let reused = manager
        .ask_question(&workspace.name, ask("Which scheduler?", &[]))
        .await
        .unwrap();
    assert_eq!(reused.id, 2);

    // An answer chosen from the question the user was shown does not reach
    // the later question reusing its number.
    let error = manager
        .answer_question(2, "cron".into(), Some(answered))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("changed since it was shown"),
        "{error:#}"
    );
    let shown = reused.clone();
    let answered = manager
        .answer_question(2, "cron".into(), Some(shown))
        .await
        .unwrap();
    assert_eq!(answered.ask, reused.ask);
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
        async move {
            manager
                .await_answer(&name, question.id, "Which database?")
                .await
        }
    });
    manager
        .answer_question(question.id, "Postgres".into(), None)
        .await
        .unwrap();
    let answered = waiter.await.unwrap().unwrap();
    assert_eq!(answered.answer.as_deref(), Some("Postgres"));

    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    let error = manager
        .await_answer(&other.name, question.id, "Which database?")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("has no question"));

    // A later question reusing the number, even within the same second, is
    // not the one the wait follows.
    manager
        .mark_agent_messages_delivered(&workspace.name, vec![answered.message_id.unwrap()])
        .await
        .unwrap();
    let reused = manager
        .ask_question(&workspace.name, ask("Which cache?", &[]))
        .await
        .unwrap();
    assert_eq!(reused.id, question.id);
    let error = manager
        .await_answer(&workspace.name, question.id, "Which database?")
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
        .answer_question(question.id, "Postgres".into(), None)
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
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
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
                question: "Which database?".into(),
            },
            Method::ListQuestions {
                workspace: Some(target.clone()),
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
    let mut listing = Method::ListQuestions { workspace: None };
    scope::authorize(&manager, Some("scope"), &mut listing)
        .await
        .unwrap();
    assert!(matches!(
        listing,
        Method::ListQuestions { workspace: Some(owner) } if owner == workspace.id
    ));
    for mut method in [
        Method::AnswerQuestion {
            id: 1,
            answer: "yes".into(),
            shown: None,
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
        match manager
            .answer_question(question.id, answer.clone(), None)
            .await
        {
            Ok(_) => unread += 1,
            Err(error) => break error,
        }
    };
    assert!(full.to_string().contains("would exceed"), "{full:#}");
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    assert_eq!(messages.len(), unread);
    let body = crate::protocol::Body::AgentMessages(messages);
    assert!(serde_json::to_vec(&body).unwrap().len() < crate::protocol::MAX_FRAME);

    // The question whose answer did not fit stays open; more open questions
    // than one response holds leave the oldest out.
    let mut open = 1;
    let listing = loop {
        manager
            .ask_question(&workspace.name, maximal(unread + open))
            .await
            .unwrap();
        open += 1;
        let listing = manager.questions(None).await.unwrap();
        if listing.omitted > 0 {
            break listing;
        }
    };
    assert_eq!(listing.omitted + listing.questions.len(), open);
    assert!(listing.questions.iter().all(Question::is_open));
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

#[tokio::test]
async fn a_workspace_reserved_for_removal_takes_no_question() {
    let (_root, manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET state=?2 WHERE id=?1",
                params![id, crate::state::WorkspaceState::Removing],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let error = manager
        .ask_question(&workspace.name, ask("Which database?", &[]))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not ready"), "{error:#}");
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
}
