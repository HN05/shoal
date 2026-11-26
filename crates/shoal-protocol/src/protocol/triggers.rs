use super::{Forwarding, ItemContext, Target};
use crate::triggers::WebhookRequest;
use serde::{Deserialize, Serialize};

/// Requests about triggers, those of the repository `context` acts in and
/// the global config's; without a context, only the global config's.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerMethod {
    /// Every trigger with its runtime state. Answers with
    /// [`super::Body::Triggers`].
    ListTriggers { context: Option<ItemContext> },
    /// Pause the trigger `name`, or resume it when not `paused`, the
    /// repository's before the global config's of that name. Answers with
    /// [`super::Body::Trigger`].
    PauseTrigger {
        context: Option<ItemContext>,
        name: String,
        paused: bool,
    },
    /// Fire the trigger `name` now, a webhook trigger with `request` or
    /// else an empty one. Answers with [`super::Body::TriggerFired`].
    RunTrigger {
        context: Option<ItemContext>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<WebhookRequest>,
    },
}

impl TriggerMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ListTriggers { .. } => true,
            Self::PauseTrigger { .. } | Self::RunTrigger { .. } => false,
        }
    }
}

impl TriggerMethod {
    /// A trigger fires on the machine of the space whose repository it is
    /// in.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ListTriggers { .. } | Self::PauseTrigger { .. } | Self::RunTrigger { .. } => {
                Forwarding::Online
            }
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ListTriggers { context }
            | Self::PauseTrigger { context, .. }
            | Self::RunTrigger { context, .. } => context
                .as_ref()
                .and_then(ItemContext::named_space)
                .map(Target::Space),
        }
    }
}
