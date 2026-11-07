//! Questions agents ask the user.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, await_reply, request},
    files::NewFile,
    item_numbers::ItemNumber,
    protocol::QuestionMethod,
};

/// What an agent asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub question: String,
    pub recommendation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// The number the user answers it by, its space's and its own; reused
    /// once the question is deleted.
    #[serde(flatten)]
    pub number: ItemNumber,
    /// The workspace or chat whose agent asked it.
    pub space_id: String,
    /// The space's current name.
    pub space: String,
    /// Unix seconds.
    pub created_at: i64,
    #[serde(flatten)]
    pub ask: Ask,
    /// `None` while the question is open.
    pub answer: Option<String>,
    pub answered_at: Option<i64>,
    /// The agent message carrying the answer; delivering it deletes the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    /// Unix seconds `done` or withdrawal closed the unanswered question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<i64>,
    /// Whether the asking agent closed it, needing no answer after all.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
    /// Whether the agent asked it in its chat, whose message is the
    /// question; it recommends nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_chat: bool,
    /// The files the agent sent with it, such as screenshots of the choices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::files::SentFile>,
}

/// Questions that fit one response, and how many older ones were left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub questions: Vec<Question>,
    pub omitted: usize,
}

/// The question `number`.
pub async fn show(daemon: &impl Daemon, number: ItemNumber) -> Result<Question> {
    request(daemon, QuestionMethod::ShowQuestion { number }).await
}

/// The open questions of `space`, or of every space.
pub async fn list(daemon: &impl Daemon, space: Option<SpaceSelector>) -> Result<Listing> {
    request(daemon, QuestionMethod::ListQuestions { space }).await
}

/// Ask the user `ask` for the agents of `space` with `attachments`; the
/// chat `sender_chat` names, when its agent asks, attaches its files.
pub async fn ask(
    daemon: &impl Daemon,
    space: SpaceSelector,
    ask: Ask,
    attachments: Vec<NewFile>,
    sender_chat: Option<String>,
) -> Result<Question> {
    let method = QuestionMethod::AskQuestion {
        space,
        ask,
        attachments,
        sender_chat,
    };
    request(daemon, method).await
}

/// The question `item` of `space`, which asks `ask`, once it is answered,
/// waiting across daemon restarts; `restarted` says the daemon restarted
/// before the wait began.
pub async fn await_answer(
    daemon: &impl Daemon,
    space: &str,
    item: u32,
    ask: &Ask,
    restarted: bool,
) -> Result<Question> {
    let method = || QuestionMethod::AwaitAnswer {
        space: SpaceSelector::parse(space),
        item,
        ask: ask.clone(),
    };
    Question::try_from(await_reply(daemon, method, restarted).await?)
}

/// Mark the turn of `space`'s calling agent as asking the user in its
/// conversation.
pub async fn ask_in_chat(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, QuestionMethod::AskInChat { space }).await
}

/// Close the questions `space`'s agents asked in their conversation, where
/// the user replied.
pub async fn replied_in_chat(daemon: &impl Daemon, space: SpaceSelector) -> Result<()> {
    request(daemon, QuestionMethod::RepliedInChat { space }).await
}

/// Withdraw `shown`, which its space's agents no longer need answered.
pub async fn withdraw(daemon: &impl Daemon, shown: Question) -> Result<Question> {
    let method = QuestionMethod::WithdrawQuestion {
        space: SpaceSelector::id(&shown.space_id),
        number: shown.number,
        shown: Some(shown),
    };
    request(daemon, method).await
}

/// Answer the question `number` with `answer`; `shown` is the question the
/// user was shown, which a later question reusing its number does not match.
pub async fn answer(
    daemon: &impl Daemon,
    number: ItemNumber,
    answer: String,
    shown: Option<Question>,
) -> Result<Question> {
    let method = QuestionMethod::AnswerQuestion {
        number,
        answer,
        shown,
    };
    request(daemon, method).await
}

/// Answer `shown` with its recommendation.
pub async fn accept_recommendation(daemon: &impl Daemon, shown: Question) -> Result<Question> {
    let method = QuestionMethod::AcceptQuestionRecommendation {
        number: shown.number,
        shown: Some(shown),
    };
    request(daemon, method).await
}
