use std::{sync::Arc, time::Duration};

use crate::{
    daemon::{
        workspace::{ExecutionKind, Manager},
        workspace_files::Attacher,
    },
    model::Workspace,
    protocol::TurnEnd,
    test_support::{manager, repository},
};

/// A workspace with a tracked agent, its execution's ID, and another
/// workspace.
type Fixture = (
    tempfile::TempDir,
    Arc<Manager>,
    Workspace,
    String,
    Workspace,
);

async fn agent() -> Fixture {
    agent_named("claude").await
}

/// [`agent`] for the agent `name`.
async fn agent_named(name: &str) -> Fixture {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .track_agent(
            &id,
            &crate::execution::TrackedAgent::named(name),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    (root, manager, workspace, id, other)
}

fn ending(message: &str) -> TurnEnd {
    TurnEnd {
        final_message: Some(message.into()),
        ..TurnEnd::default()
    }
}

#[tokio::test]
async fn a_marked_turn_asks_its_final_message_until_the_user_replies_in_the_chat() {
    let (_root, manager, workspace, id, _other) = agent().await;
    let end_turn = async |message| {
        manager
            .end_agent_turn(&workspace.name, Some(id.clone()), None, ending(message))
            .await
            .unwrap();
    };
    // An unmarked turn asks nothing.
    end_turn("Done.").await;
    assert!(manager.questions(None).await.unwrap().questions.is_empty());

    manager
        .mark_chat_question(&workspace.name, Some(&id))
        .await
        .unwrap();
    end_turn("Which database should the cache use?\n\nI recommend SQLite.").await;
    let asked = manager.questions(None).await.unwrap().questions;
    assert_eq!(asked.len(), 1);
    assert!(asked[0].in_chat);
    assert_eq!(
        asked[0].ask.question,
        "Which database should the cache use?\n\nI recommend SQLite."
    );
    let notification = &manager.notifications(true, 50).await.unwrap()[0];
    assert_eq!(
        notification.message,
        format!(
            "question {0} (answer with shoal questions {0}): Which database should the cache use?  I recommend SQLite.",
            asked[0].id
        )
    );
    // The mark lasts one turn.
    end_turn("Waiting for your answer.").await;
    assert_eq!(manager.questions(None).await.unwrap().questions.len(), 1);

    let closed = manager
        .reply_in_chat(&workspace.name, Some(&id))
        .await
        .unwrap();
    assert_eq!(closed, [asked[0].id]);
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
    // The user had not read it, so the notification goes with it.
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn asking_the_same_in_the_chat_again_consumes_the_mark() {
    let (_root, manager, workspace, id, _other) = agent().await;
    let ask_in_chat = async |message| {
        manager
            .mark_chat_question(&workspace.name, Some(&id))
            .await
            .unwrap();
        manager
            .end_agent_turn(&workspace.name, Some(id.clone()), None, ending(message))
            .await
            .unwrap();
    };
    ask_in_chat("Which database should the cache use?").await;
    ask_in_chat("Which database should the cache use?").await;
    let asked = manager.questions(None).await.unwrap().questions;
    assert_eq!(asked.len(), 1);
    assert_eq!(manager.notifications(true, 50).await.unwrap().len(), 1);

    // The repeat consumed the mark, so the next turn asks nothing.
    manager
        .end_agent_turn(
            &workspace.name,
            Some(id.clone()),
            None,
            ending("Waiting for your answer."),
        )
        .await
        .unwrap();
    assert_eq!(manager.questions(None).await.unwrap().questions, asked);
}

#[tokio::test]
async fn an_inbox_answer_reaches_the_agent_without_repeating_its_message() {
    let (_root, manager, workspace, id, _other) = agent().await;
    manager
        .mark_chat_question(&workspace.name, Some(&id))
        .await
        .unwrap();
    manager
        .end_agent_turn(&workspace.name, Some(id.clone()), None, TurnEnd::default())
        .await
        .unwrap();
    let question = manager.questions(None).await.unwrap().questions.remove(0);
    // A hook that passes no final message still reaches the user.
    assert!(
        question
            .ask
            .question
            .contains("asked a question in its chat"),
        "{}",
        question.ask.question
    );
    manager
        .answer_question(question.id, "SQLite".into(), None)
        .await
        .unwrap();
    let messages = manager.agent_messages(&workspace.name).await.unwrap();
    assert_eq!(
        messages[0].message,
        format!(
            "Answer to question {}, which you asked in the chat: SQLite",
            question.id
        )
    );
}

#[tokio::test]
async fn a_reply_during_the_turn_clears_its_mark() {
    let (_root, manager, workspace, id, _other) = agent().await;
    manager
        .mark_chat_question(&workspace.name, Some(&id))
        .await
        .unwrap();
    manager
        .reply_in_chat(&workspace.name, Some(&id))
        .await
        .unwrap();
    manager
        .end_agent_turn(&workspace.name, Some(id), None, ending("Which one?"))
        .await
        .unwrap();
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
}

#[tokio::test]
async fn only_the_workspaces_own_agent_asks_in_its_chat() {
    let (_root, manager, _workspace, id, other) = agent().await;
    for execution in [Some(id.as_str()), None] {
        let error = manager
            .mark_chat_question(&other.name, execution)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("asks in its chat"), "{error:#}");
    }
}

#[tokio::test]
async fn a_mark_outlasts_the_agent_reconnecting_to_a_restarted_daemon() {
    let (_root, manager, workspace, id, _other) = agent().await;
    manager
        .mark_chat_question(&workspace.name, Some(&id))
        .await
        .unwrap();
    // Reattachment tracks the agent anew.
    manager
        .track_agent(
            &id,
            &crate::execution::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    manager
        .end_agent_turn(&workspace.name, Some(id), None, ending("Which one?"))
        .await
        .unwrap();
    assert_eq!(manager.questions(None).await.unwrap().questions.len(), 1);
}

#[tokio::test]
async fn an_agent_without_shoals_turn_end_hook_asks_on_the_command_line() {
    let (_root, manager, workspace, id, _other) = agent_named("custom").await;
    let error = manager
        .mark_chat_question(&workspace.name, Some(&id))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("has no Shoal hook that ends its turns"),
        "{error:#}"
    );
}

#[tokio::test]
async fn a_coordinator_that_received_the_question_hears_a_reply_closed_it() {
    use crate::state::AgentState;
    let (_root, manager, parent, _id, _other) = agent().await;
    manager
        .set_agent_state(&parent.id, AgentState::Working, None, None)
        .await
        .unwrap();
    let child = manager
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(&child.id, None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let id = execution.plan.id.clone();
    manager
        .track_agent(
            &id,
            &crate::execution::TrackedAgent::named("claude"),
            &child.id,
            &child.name,
            false,
            Duration::ZERO,
        )
        .await;
    manager
        .set_agent_state(&child.id, AgentState::Working, Some(id.clone()), None)
        .await
        .unwrap();
    manager
        .mark_chat_question(&child.name, Some(&id))
        .await
        .unwrap();
    manager
        .end_agent_turn(&child.name, Some(id.clone()), None, ending("Which one?"))
        .await
        .unwrap();
    let question = manager.questions(None).await.unwrap().questions.remove(0);
    manager.reply_in_chat(&child.name, Some(&id)).await.unwrap();
    let received: Vec<_> = manager
        .agent_messages(&parent.id)
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect();
    assert_eq!(
        received,
        [
            format!(
                "child: question {0} (answer with shoal questions {0}): Which one?",
                question.id
            ),
            format!(
                "child: withdrew question {} (no answer needed): Which one?",
                question.id
            ),
        ]
    );
    assert!(manager.notifications(true, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_chats_agent_asks_hears_the_answer_in_its_inbox_and_asks_in_its_conversation() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let ask = crate::daemon::questions::Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    let asked = manager
        .ask_question(&chat.name, ask, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    assert_eq!((&*asked.space_id, &*asked.space), (&*chat.id, "planning"));
    let listed = manager.questions(Some(&chat.name)).await.unwrap();
    assert_eq!(listed.questions, std::slice::from_ref(&asked));
    // The answer waits in the chat's inbox, and its delivery deletes the
    // question.
    manager
        .answer_question(asked.id, "Postgres".into(), None)
        .await
        .unwrap();
    let messages = manager.chat_messages(&chat.id).await.unwrap();
    assert_eq!(
        messages[0].message,
        format!(
            "Answer to question {} (Which database?): Postgres",
            asked.id
        )
    );
    manager
        .mark_chat_messages_delivered(&chat.id, vec![messages[0].id])
        .await
        .unwrap();
    assert!(manager.question(asked.id).await.is_err());

    // The chat marks its own turn, which no execution tracks.
    manager.mark_chat_question(&chat.name, None).await.unwrap();
    manager
        .end_agent_turn(&chat.name, None, None, ending("Which cache?"))
        .await
        .unwrap();
    let asked = manager.questions(None).await.unwrap().questions;
    assert_eq!(asked.len(), 1);
    assert!(asked[0].in_chat);
    assert_eq!(asked[0].space, "planning");
    let closed = manager.reply_in_chat(&chat.name, None).await.unwrap();
    assert_eq!(closed, [asked[0].id]);

    // An open question stays while the chat is stopped and goes with it.
    manager.mark_chat_question(&chat.name, None).await.unwrap();
    manager
        .end_agent_turn(&chat.name, None, None, ending("Which queue?"))
        .await
        .unwrap();
    assert_eq!(manager.questions(None).await.unwrap().questions.len(), 1);
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    assert_eq!(manager.questions(None).await.unwrap().questions.len(), 1);
    manager.remove_chat(&chat.id, None).await.unwrap();
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
}
