use super::ItemContext;
use crate::triggers::TriggerAction;
use serde::{Deserialize, Serialize};

/// Requests about triggers, those of the repository `context` acts in and
/// the global config's; without a context, only the global config's.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMethod {
    /// Every trigger with its runtime state. Answers with
    /// [`super::Body::Triggers`].
    ListTriggers { context: Option<ItemContext> },
    /// Pause, resume or run the trigger `name`, the repository's before
    /// the global config's of that name. Answers with
    /// [`super::Body::Trigger`], or with [`super::Body::TriggerFired`] for
    /// a run.
    ActOnTrigger {
        context: Option<ItemContext>,
        name: String,
        action: TriggerAction,
    },
}
