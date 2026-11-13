use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::item_numbers::ItemNumber;
use crate::space_names::SpaceSelector;

/// Requests about agents' questions to the user and their answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionMethod {
    /// A question from a space's agents to the user; asking an identical
    /// question again returns it until its answer is delivered.
    AskQuestion {
        space: SpaceSelector,
        ask: crate::questions::Ask,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
        /// The chat whose agent asks, which attaches its files.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_chat: Option<SpaceId>,
    },
    /// Open questions, in every space unless one is named.
    ListQuestions { space: Option<SpaceSelector> },
    /// One question by number, for the user.
    ShowQuestion { number: ItemNumber },
    /// Close the space's open question `number`, which its agents no
    /// longer need answered.
    WithdrawQuestion {
        space: SpaceSelector,
        number: ItemNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::questions::Question>,
    },
    /// The user's answer, queued for the asking space's agents.
    /// `shown` is the question the user was shown, which a later question
    /// reusing its number does not match.
    AnswerQuestion {
        number: ItemNumber,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shown: Option<crate::questions::Question>,
    },
    /// Accept the stored recommendation literally, including numeric text.
    AcceptQuestionRecommendation {
        number: ItemNumber,
        shown: Option<crate::questions::Question>,
    },
    /// The caller's agent asks the user in its conversation this turn: the
    /// turn's final message becomes a question.
    AskInChat { space: SpaceSelector },
    /// The user replied in the conversation of the caller's agent, which
    /// closes the questions the space's agents asked there.
    RepliedInChat { space: SpaceSelector },
    /// Long-lived: the question `item` of the space once it is answered.
    /// What it asks tells it from a later question reusing its number.
    AwaitAnswer {
        space: SpaceSelector,
        item: u32,
        ask: crate::questions::Ask,
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
