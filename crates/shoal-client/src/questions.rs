//! Questions agents ask the user.
use crate::space_ids::SpaceId;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::space_names::{SpaceName, SpaceSelector};
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
    pub space_id: SpaceId,
    /// The space's current name.
    pub space: SpaceName,
    pub created_at: crate::time::UnixTime,
    #[serde(flatten)]
    pub ask: Ask,
    #[serde(flatten)]
    pub status: QuestionStatus,
    /// Whether the agent asked it in its chat, whose message is the
    /// question; it recommends nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_chat: bool,
    /// The files the agent sent with it, such as screenshots of the choices.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::files::SentFile>,
}

impl Question {
    /// The question on one line, as listings and notifications show it: a
    /// question asked in the chat may span lines.
    pub fn one_line(&self) -> String {
        self.ask.question.replace(['\n', '\t'], " ")
    }

    /// The answer the agent recommends; a question asked in the chat has
    /// none.
    pub fn recommendation(&self) -> Option<&str> {
        (!self.in_chat).then_some(self.ask.recommendation.as_str())
    }

    pub fn is_open(&self) -> bool {
        self.status == QuestionStatus::Open
    }

    /// Refuse an answer to a question that is answered or closed.
    pub fn ensure_open(&self) -> Result<()> {
        let number = self.number;
        match self.status {
            QuestionStatus::Open => Ok(()),
            QuestionStatus::Answered { .. } => bail!("question {number} is already answered"),
            QuestionStatus::Withdrawn { .. } => {
                bail!("question {number} was withdrawn: its agent no longer needs an answer")
            }
            QuestionStatus::Closed { .. } => {
                bail!("question {number} is closed: its workspace's assignment was marked done")
            }
        }
    }

    /// Whether `other` is this question as asked, though it may since be
    /// answered: the same space, ask and time.
    pub fn is_same(&self, other: &Question) -> bool {
        self.space_id == other.space_id
            && self.ask == other.ask
            && self.created_at == other.created_at
    }

    /// What the user is told about a new question, including how to answer it.
    /// It is cut to the length of other messages, so a batch of notifications
    /// still fits one response.
    pub fn notification(&self) -> String {
        let mut text = format!(
            "question {} (answer with shoal questions {}): {}",
            self.number,
            self.number,
            self.one_line()
        );
        if let Some(recommendation) = self.recommendation() {
            text.push_str(&format!(" Recommended: {recommendation}"));
        }
        crate::validate::shorten(text, crate::validate::MAX_MESSAGE_LEN)
    }

    /// What the user, or the coordinating agent, is told about a withdrawn
    /// question whose notification was already shown.
    pub fn withdrawal_notification(&self) -> String {
        crate::validate::shorten(
            format!(
                "withdrew question {} (no answer needed): {}",
                self.number,
                self.one_line()
            ),
            crate::validate::MAX_MESSAGE_LEN,
        )
    }

    /// The agent message carrying `answer`; it names the question it answers.
    pub fn answer_message(&self, answer: &str) -> String {
        if self.in_chat {
            return format!(
                "Answer to question {}, which you asked in the chat: {answer}",
                self.number
            );
        }
        format!(
            "Answer to question {} ({}): {answer}",
            self.number, self.ask.question
        )
    }
}

/// Whether a question waits for an answer, and how it stopped waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QuestionStatus {
    Open,
    Answered {
        answer: String,
        answered_at: crate::time::UnixTime,
        /// The agent message carrying the answer, until it is delivered;
        /// delivering it deletes the question.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<i64>,
    },
    /// `done` closed it unanswered.
    Closed {
        closed_at: crate::time::UnixTime,
    },
    /// The asking agent closed it, needing no answer after all, or the user
    /// replied in the chat it was asked in.
    Withdrawn {
        withdrawn_at: crate::time::UnixTime,
    },
}

impl QuestionStatus {
    /// The user's answer, once there is one.
    pub fn answer(&self) -> Option<&str> {
        match self {
            Self::Answered { answer, .. } => Some(answer),
            Self::Open | Self::Closed { .. } | Self::Withdrawn { .. } => None,
        }
    }
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
    sender_chat: Option<SpaceId>,
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
    space: &SpaceSelector,
    item: u32,
    ask: &Ask,
    restarted: bool,
) -> Result<Question> {
    let method = || QuestionMethod::AwaitAnswer {
        space: space.clone(),
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

#[cfg(test)]
mod tests;
