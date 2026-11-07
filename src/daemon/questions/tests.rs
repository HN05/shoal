use super::*;
use crate::client::assignments::Done;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        scope::{self, Caller},
        test_support::{manager, repository},
        workspace_files::Attacher,
    },
    model::{CleanupReason, Workspace},
    protocol::QuestionMethod,
    removal::{InspectionPolicy, UnsavedWork},
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

/// Item `item` of the space numbered `space`.
fn item(space: u32, item: u32) -> ItemNumber {
    ItemNumber { space, item }
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
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let again = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(again, first);
    assert!(first.is_open());
    assert_eq!(first.space, "asker");

    let notifications = manager.notifications(true, 50).await.unwrap();
    let asked: Vec<_> = notifications
        .iter()
        .filter(|n| n.kind == NotificationKind::QuestionAsked)
        .collect();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0].message.contains(&first.number.to_string()),
        "{}",
        asked[0].message
    );

    assert!(
        manager
            .ask_question(
                &SpaceSelector::from(&workspace.name),
                ask("Which database?", &["SQLite"]),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .ask_question(
                &SpaceSelector::from(&workspace.name),
                ask("two\nlines", &[]),
                Vec::new(),
                &Attacher::User
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn answers_resolve_choices_and_reach_the_agents_once() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &["SQLite", "Postgres"]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    for invalid in ["3", "0", " "] {
        assert!(
            manager
                .answer_question(question.number, invalid.into(), None)
                .await
                .is_err(),
            "{invalid:?}"
        );
    }
    let answered = manager
        .answer_question(question.number, "2".into(), None)
        .await
        .unwrap();
    assert_eq!(answered.answer.as_deref(), Some("Postgres"));
    let messages = manager
        .agent_messages(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(answered.message_id, Some(messages[0].id));
    assert!(
        messages[0].message.contains("Postgres"),
        "{}",
        messages[0].message
    );
    assert!(
        manager
            .answer_question(question.number, "1".into(), None)
            .await
            .is_err()
    );

    let open = manager
        .questions(Some(&SpaceSelector::from(&workspace.name)))
        .await
        .unwrap();
    assert!(open.questions.is_empty());
    assert_eq!(manager.question(question.number).await.unwrap(), answered);
}

#[tokio::test]
async fn recommended_numeric_answers_are_literal_and_bound_to_the_shown_question() {
    let (_root, manager, workspace) = fixture().await;
    let mut request = ask("Which number?", &["2", "4"]);
    request.recommendation = "2".into();
    let question = manager
        .ask_question(
            &SpaceSelector::id(&workspace.id),
            request,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let mut stale = question.clone();
    stale.ask.recommendation = "4".into();
    assert!(
        manager
            .accept_question_recommendation(question.number, Some(stale))
            .await
            .is_err()
    );
    let answered = manager
        .accept_question_recommendation(question.number, Some(question))
        .await
        .unwrap();
    assert_eq!(answered.answer.as_deref(), Some("2"));
    assert_eq!(
        manager
            .agent_messages(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn free_text_answers_may_span_lines() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let answered = manager
        .answer_question(
            question.number,
            "Postgres,\nsince staging has it".into(),
            None,
        )
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
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let answered = manager
        .answer_question(question.number, "Postgres".into(), None)
        .await
        .unwrap();
    let again = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(again, answered);

    let message = answered.message_id.unwrap();
    manager
        .mark_agent_messages_delivered(&SpaceSelector::from(&workspace.name), vec![message])
        .await
        .unwrap();
    // Delivery deletes the question and frees its number.
    assert!(manager.question(question.number).await.is_err());
    let new = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(new.number, question.number);
    assert!(new.is_open());
}

#[tokio::test]
async fn questions_take_the_smallest_free_number_of_their_space() {
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
            .ask_question(
                &SpaceSelector::from(&asker.name),
                ask(text, &[]),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        numbers.push(question.number);
    }
    let (first, second) = (workspace.number, other.number);
    assert_eq!(numbers, [item(first, 1), item(second, 1), item(first, 2)]);

    // An answer keeps its number until the agent receives it.
    let answered = manager
        .answer_question(item(second, 1), "Redis".into(), None)
        .await
        .unwrap();
    let next = manager
        .ask_question(
            &SpaceSelector::from(&other.name),
            ask("Which logger?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(next.number, item(second, 2));
    manager
        .mark_agent_messages_delivered(
            &SpaceSelector::from(&other.name),
            vec![answered.message_id.unwrap()],
        )
        .await
        .unwrap();
    let reused = manager
        .ask_question(
            &SpaceSelector::from(&other.name),
            ask("Which scheduler?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(reused.number, item(second, 1));

    // An answer chosen from the question the user was shown does not reach
    // the later question reusing its number.
    assert!(
        manager
            .answer_question(item(second, 1), "cron".into(), Some(answered))
            .await
            .is_err()
    );
    let shown = reused.clone();
    let answered = manager
        .answer_question(item(second, 1), "cron".into(), Some(shown))
        .await
        .unwrap();
    assert_eq!(answered.ask, reused.ask);
}

#[tokio::test]
async fn a_wait_returns_the_answer() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let waiter = tokio::spawn({
        let manager = manager.clone();
        let name = workspace.name.clone();
        let ask = question.ask.clone();
        async move {
            manager
                .await_answer(&SpaceSelector::from(&name), question.number.item, &ask)
                .await
        }
    });
    manager
        .answer_question(question.number, "Postgres".into(), None)
        .await
        .unwrap();
    let answered = waiter.await.unwrap().unwrap();
    assert_eq!(answered.answer.as_deref(), Some("Postgres"));

    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    assert!(
        manager
            .await_answer(
                &SpaceSelector::from(&other.name),
                question.number.item,
                &question.ask
            )
            .await
            .is_err()
    );

    // A later question reusing the number, even within the same second and
    // with the same text, is not the one the wait follows when it asks
    // anything differently.
    manager
        .mark_agent_messages_delivered(
            &SpaceSelector::from(&workspace.name),
            vec![answered.message_id.unwrap()],
        )
        .await
        .unwrap();
    let reused = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &["SQLite"]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(reused.number, question.number);
    assert!(
        manager
            .await_answer(
                &SpaceSelector::from(&workspace.name),
                question.number.item,
                &question.ask
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn open_questions_keep_the_workspace_and_removal_deletes_them() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Question
    );
    manager
        .answer_question(question.number, "Postgres".into(), None)
        .await
        .unwrap();
    assert_eq!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Idle { minutes: 10 }
    );

    manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which cache?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
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
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
        for mut method in [
            QuestionMethod::AskQuestion {
                space: SpaceSelector::parse(target),
                ask: ask("Which database?", &[]),
                attachments: Vec::new(),
                sender_chat: None,
            },
            QuestionMethod::AwaitAnswer {
                space: SpaceSelector::parse(target),
                item: 1,
                ask: ask("Which database?", &[]),
            },
            QuestionMethod::ListQuestions {
                space: Some(SpaceSelector::parse(target)),
            },
            QuestionMethod::WithdrawQuestion {
                space: SpaceSelector::parse(target),
                number: item(1, 1),
                shown: None,
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
    let mut listing = QuestionMethod::ListQuestions { space: None };
    scope::authorize(&manager, Some("scope"), &mut listing)
        .await
        .unwrap();
    assert!(matches!(
        listing,
        QuestionMethod::ListQuestions { space: Some(owner) } if owner == SpaceSelector::id(&workspace.id)
    ));
    for mut method in [
        QuestionMethod::AnswerQuestion {
            number: item(1, 1),
            answer: "yes".into(),
            shown: None,
        },
        QuestionMethod::ShowQuestion { number: item(1, 1) },
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
    // Answer until one no longer fits the unread answers.
    loop {
        let question = manager
            .ask_question(
                &SpaceSelector::from(&workspace.name),
                maximal(unread),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        match manager
            .answer_question(question.number, answer.clone(), None)
            .await
        {
            Ok(_) => unread += 1,
            Err(_) => break,
        }
    }
    assert!(unread > 0);
    let messages = manager
        .agent_messages(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert_eq!(messages.len(), unread);
    let body = crate::protocol::Body::AgentMessages(messages);
    assert!(serde_json::to_vec(&body).unwrap().len() < crate::protocol::MAX_FRAME);

    // The question whose answer did not fit stays open; more open questions
    // than one response holds leave the oldest out.
    let mut open = 1;
    let listing = loop {
        manager
            .ask_question(
                &SpaceSelector::from(&workspace.name),
                maximal(unread + open),
                Vec::new(),
                &Attacher::User,
            )
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
            .ask_question(
                &SpaceSelector::from(&workspace.name),
                maximal(n),
                Vec::new(),
                &Attacher::User,
            )
            .await
            .unwrap();
        first.get_or_insert(question);
    }
    let notifications = manager.notifications(true, 50).await.unwrap();
    assert_eq!(notifications.len(), 50);
    let first = first.unwrap();
    let message = &notifications[0].message;
    assert!(message.len() <= crate::validate::MAX_MESSAGE_LEN);
    assert!(message.ends_with('…'));
    let body = crate::protocol::Body::Notifications(notifications);
    assert!(serde_json::to_vec(&body).unwrap().len() < crate::protocol::MAX_FRAME);

    assert_eq!(manager.question(first.number).await.unwrap(), first);
    let absent = item(first.number.space, first.number.item + 100);
    assert!(manager.question(absent).await.is_err());
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
    let asked = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await;
    assert!(asked.is_err());
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
}

#[tokio::test]
async fn done_closes_open_questions_for_good() {
    let (_root, manager, workspace) = fixture().await;
    let answered = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .answer_question(answered.number, "Postgres".into(), None)
        .await
        .unwrap();
    let open = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which cache?", &[]),
            Vec::new(),
            &Attacher::User,
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
        .unwrap();

    assert!(manager.questions(None).await.unwrap().questions.is_empty());
    assert_ne!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Question
    );
    let notifications = manager.notifications(true, 50).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|n| n.kind == NotificationKind::SpaceDone
                && n.message.contains(&open.number.to_string()))
    );
    // The undelivered answer still reaches the agents.
    let delivered = manager.question(answered.number).await.unwrap();
    assert_eq!(delivered.answer.as_deref(), Some("Postgres"));
    assert!(delivered.closed_at.is_none());

    assert!(
        manager
            .answer_question(open.number, "Redis".into(), None)
            .await
            .is_err()
    );
    assert!(
        manager
            .await_answer(
                &SpaceSelector::from(&workspace.name),
                open.number.item,
                &open.ask
            )
            .await
            .is_err()
    );

    // Withdrawing completion leaves it closed, and its number stays taken.
    manager
        .undo_done(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert!(!manager.has_open_questions(&workspace.id).await.unwrap());
    let asked_again = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which cache?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(asked_again.is_open());
    assert_ne!(asked_again.number, open.number);
}

#[tokio::test]
async fn withdrawing_a_displayed_question_refuses_a_reused_number() {
    let (_root, manager, workspace) = fixture().await;
    let shown = manager
        .ask_question(
            &SpaceSelector::id(&workspace.id),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let answered = manager
        .answer_question(shown.number, "SQLite".into(), None)
        .await
        .unwrap();
    manager
        .mark_agent_messages_delivered(
            &SpaceSelector::id(&workspace.id),
            vec![answered.message_id.unwrap()],
        )
        .await
        .unwrap();
    let replacement = manager
        .ask_question(
            &SpaceSelector::id(&workspace.id),
            ask("Which cache?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(shown.number, replacement.number);
    assert!(
        manager
            .withdraw_question(&SpaceSelector::id(&workspace.id), shown.number, Some(shown))
            .await
            .is_err()
    );
    assert!(
        manager
            .question(replacement.number)
            .await
            .unwrap()
            .is_open()
    );
    let withdrawn = manager
        .withdraw_question(
            &SpaceSelector::id(&workspace.id),
            replacement.number,
            Some(replacement),
        )
        .await
        .unwrap();
    assert!(withdrawn.withdrawn);
}

#[tokio::test]
async fn withdrawing_closes_a_question_and_takes_back_its_unread_notification() {
    let (_root, manager, workspace) = fixture().await;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let waiter = tokio::spawn({
        let manager = manager.clone();
        let name = workspace.name.clone();
        let ask = question.ask.clone();
        async move {
            manager
                .await_answer(&SpaceSelector::from(&name), question.number.item, &ask)
                .await
        }
    });
    assert!(
        manager
            .withdraw_question(&SpaceSelector::from(&other.name), question.number, None)
            .await
            .is_err()
    );

    let withdrawn = manager
        .withdraw_question(&SpaceSelector::from(&workspace.name), question.number, None)
        .await
        .unwrap();
    assert!(withdrawn.withdrawn && !withdrawn.is_open());
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
    assert_ne!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Question
    );
    // The user was never shown it, so nothing tells them about it.
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
    assert!(waiter.await.unwrap().is_err());
    assert!(
        manager
            .answer_question(question.number, "Postgres".into(), None)
            .await
            .is_err()
    );
    assert!(
        manager
            .withdraw_question(&SpaceSelector::from(&workspace.name), question.number, None)
            .await
            .is_err()
    );
    // Its number stays taken, and asking again asks anew.
    let again = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(again.is_open());
    assert_ne!(again.number, question.number);
}

#[tokio::test]
async fn withdrawing_after_a_rename_still_takes_back_the_notification() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .rename_workspace(&SpaceSelector::id(&workspace.id), "renamed", None)
        .await
        .unwrap();
    manager
        .withdraw_question(&SpaceSelector::id(&workspace.id), question.number, None)
        .await
        .unwrap();
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn withdrawing_a_shown_question_tells_the_user() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let shown = manager.notifications(true, 50).await.unwrap();
    manager
        .mark_notifications_read(shown.iter().map(|n| n.id).collect())
        .await
        .unwrap();
    manager
        .withdraw_question(&SpaceSelector::from(&workspace.name), question.number, None)
        .await
        .unwrap();
    let unread = manager.notifications(true, 50).await.unwrap();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].kind, NotificationKind::QuestionWithdrawn);
}

#[tokio::test]
async fn a_withdrawn_question_is_never_announced_afterwards() {
    let (_root, manager, workspace) = fixture().await;
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which database?", &[]),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .withdraw_question(&SpaceSelector::from(&workspace.name), question.number, None)
        .await
        .unwrap();
    // An announcement that lost the race to the withdrawal comes too late.
    manager.announce_question(&question).await;
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn files_sent_with_a_question_are_listed_with_it_and_its_notification() {
    let (root, manager, workspace) = fixture().await;
    let layouts: Vec<NewFile> = ["a.png", "b.png"]
        .into_iter()
        .map(|name| {
            let path = root.path().join(name);
            std::fs::write(&path, name).unwrap();
            path.into()
        })
        .collect();
    let asked = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which layout?", &["A", "B"]),
            layouts.clone(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let names: Vec<_> = asked.files.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["a.png", "b.png"]);
    for file in &asked.files {
        assert_eq!(std::fs::read_to_string(&file.path).unwrap(), file.name);
    }
    let listed = manager.questions(None).await.unwrap().questions;
    assert_eq!(listed, std::slice::from_ref(&asked));
    let attached = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert!(
        attached
            .iter()
            .all(|file| file.source == FileSource::Question),
        "{attached:?}"
    );
    let notifications = manager.notifications(true, 50).await.unwrap();
    let [notification] = notifications.as_slice() else {
        panic!("{notifications:?}");
    };
    assert_eq!(notification.files, asked.files);

    // The same question with the same files is the same question; with
    // other files it is a different one.
    let again = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which layout?", &["A", "B"]),
            layouts.clone(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(again, asked);
    let asked_otherwise = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which layout?", &["A", "B"]),
            layouts[..1].to_vec(),
            &Attacher::User,
        )
        .await;
    assert!(asked_otherwise.is_err());
    // The refused question's copies go, and the asked one's stay.
    let after = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert_eq!(after, attached);
    let workspace_results = asked.files[0].path.ancestors().nth(3).unwrap();
    let messages = std::fs::read_dir(workspace_results).unwrap().count();
    assert_eq!(messages, 1, "{workspace_results:?}");
    assert!(asked.files.iter().all(|file| file.path.exists()));
}

#[tokio::test]
async fn a_question_sends_a_limited_number_of_files() {
    let (root, manager, workspace) = fixture().await;
    let shot = root.path().join("shot.png");
    std::fs::write(&shot, "png").unwrap();
    let files = vec![NewFile::from(shot); crate::client::files::MAX_SENT + 1];
    let asked = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask("Which?", &[]),
            files,
            &Attacher::User,
        )
        .await;
    assert!(asked.is_err());
    assert!(
        manager
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
}
