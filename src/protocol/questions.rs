use serde::{Deserialize, Serialize};

use super::Lifetime;

/// Requests about agents' questions to the user and their answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionMethod {
    /// A question from a workspace's agents to the user; asking an identical
    /// question again returns it until its answer is delivered.
    AskQuestion {
        workspace: String,
        ask: crate::daemon::questions::Ask,
    },
    /// Open questions, in every workspace unless one is named.
    ListQuestions { workspace: Option<String> },
    /// One question by ID, for the user.
    ShowQuestion { id: i64 },
    /// Close the workspace's open question `id`, which its agents no longer
    /// need answered.
    WithdrawQuestion {
        workspace: String,
        id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::daemon::questions::Question>,
    },
    /// The user's answer, queued for the asking workspace's agents.
    /// `shown` is the question the user was shown, which a later question
    /// reusing its number does not match.
    AnswerQuestion {
        id: i64,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::daemon::questions::Question>,
    },
    /// Accept the stored recommendation literally, including numeric text.
    AcceptQuestionRecommendation {
        id: i64,
        shown: Option<crate::daemon::questions::Question>,
    },
    /// The caller's agent asks the user in its chat this turn: the turn's
    /// final message becomes a question.
    AskInChat { workspace: String },
    /// The user replied in the chat of the caller's agent, which closes the
    /// questions the workspace's agents asked there.
    RepliedInChat { workspace: String },
    /// Long-lived: the question once it is answered. What it asks tells it
    /// from a later question reusing its number.
    AwaitAnswer {
        workspace: String,
        id: i64,
        ask: crate::daemon::questions::Ask,
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
