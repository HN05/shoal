//! Questions agents ask the user.
use serde::{Deserialize, Serialize};

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
    /// The number the user answers it by; reused once the question is deleted.
    pub id: i64,
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
}

/// Questions that fit one response, and how many older ones were left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub questions: Vec<Question>,
    pub omitted: usize,
}
