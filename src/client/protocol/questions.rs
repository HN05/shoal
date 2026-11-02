use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about agents' questions to the user and their answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionMethod {
    /// A question from a space's agents to the user; asking an identical
    /// question again returns it until its answer is delivered.
    AskQuestion {
        space: String,
        ask: crate::client::questions::Ask,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::client::files::NewFile>,
        /// The chat whose agent asks, which attaches its files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<String>,
    },
    /// Open questions, in every space unless one is named.
    ListQuestions { space: Option<String> },
    /// One question by ID, for the user.
    ShowQuestion { id: i64 },
    /// Close the space's open question `id`, which its agents no longer
    /// need answered.
    WithdrawQuestion {
        space: String,
        id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::client::questions::Question>,
    },
    /// The user's answer, queued for the asking space's agents.
    /// `shown` is the question the user was shown, which a later question
    /// reusing its number does not match.
    AnswerQuestion {
        id: i64,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::client::questions::Question>,
    },
    /// Accept the stored recommendation literally, including numeric text.
    AcceptQuestionRecommendation {
        id: i64,
        shown: Option<crate::client::questions::Question>,
    },
    /// The caller's agent asks the user in its conversation this turn: the
    /// turn's final message becomes a question.
    AskInChat { space: String },
    /// The user replied in the conversation of the caller's agent, which
    /// closes the questions the space's agents asked there.
    RepliedInChat { space: String },
    /// Long-lived: the question once it is answered. What it asks tells it
    /// from a later question reusing its number.
    AwaitAnswer {
        space: String,
        id: i64,
        ask: crate::client::questions::Ask,
    },
}

impl QuestionMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::AwaitAnswer { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
