use crate::space_names::SpaceSelector;
use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};

/// Requests about alerts: one-way notices to the user or a space's agent.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertMethod {
    /// The alerts for `to`, oldest first: the oldest unread ones when
    /// `unread_only`, so a backlog is shown in order across calls, otherwise
    /// the newest `limit`.
    ListAlerts {
        to: AlertsFor,
        unread_only: bool,
        limit: u32,
    },
    /// Mark exactly these alerts read, each one for the caller's space or,
    /// as the caller may read them, for the user.
    MarkAlertsRead { ids: Vec<i64> },
    /// Deletes every alert to the user this machine knows of, read or not,
    /// its own and the other joined machines'; answers with how many.
    ClearAlerts,
    /// Long-lived: the user's unread alerts, this machine's and the other
    /// joined machines', that `after` has not passed, then new ones as they
    /// come, oldest first, each as a [`super::Body::Alert`] response and
    /// marked read on delivery unless `keep_unread`.
    WatchAlerts {
        keep_unread: bool,
        after: crate::alerts::AlertCursor,
    },
    /// An alert from the caller, answered with where it went.
    SendAlert {
        to: AlertTarget,
        /// The space the user acts as, as a session in it that Shoal did not
        /// start does; a scoped caller always acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
        message: String,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
    },
    /// The caller's agent sends its turn's final message as an alert to
    /// `to` once the turn ends, with copies of `attachments` made then;
    /// answered with the mark and the one it replaced.
    AlertInChat {
        to: AlertTarget,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
    },
    /// Delete the unread alert `id` the caller sent, to the space `space`
    /// when it names one, which finds the alert on that space's machine.
    WithdrawAlert {
        id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        /// The space the user acts as, as a session in it that Shoal did not
        /// start does; a scoped caller always acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
}

impl AlertMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WatchAlerts { .. } => Lifetime::Open,
            _ => Lifetime::Reply,
        }
    }
}

/// Whose alerts a listing reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertsFor {
    /// The user's.
    User,
    /// Those for the agent of a space.
    Space { space: SpaceSelector },
    /// The user's, as the caller may read them, and those of every space the
    /// caller's scope reaches.
    Every,
}

/// Where an alert goes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertTarget {
    /// Up from the caller's space: to its coordinator, the agent of the
    /// nearest space above with an agent, or else the user.
    Up,
    /// Down to the agent of a space the caller's scope reaches.
    Space { space: SpaceSelector },
    /// Down to every agent the caller's scope reaches but its own, of one
    /// repository when it names one.
    All {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository: Option<crate::repository::Selector>,
    },
}

impl AlertMethod {
    /// Where the request runs when its space is another machine's: a
    /// message or a withdrawal goes to the recipient's machine, and a
    /// message waits for it while it is offline; the caller's own agent's
    /// turn is this machine's own, and the user's alerts name no space,
    /// since every machine keeps the other machines' as the hub sends
    /// them and sends the hub what reads, clears or withdraws another
    /// machine's itself.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::SendAlert { .. } => Forwarding::Queued,
            Self::ListAlerts { .. } | Self::WithdrawAlert { .. } => Forwarding::Online,
            Self::AlertInChat { .. }
            | Self::MarkAlertsRead { .. }
            | Self::ClearAlerts
            | Self::WatchAlerts { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ListAlerts {
                to: AlertsFor::Space { space },
                ..
            }
            | Self::SendAlert {
                to: AlertTarget::Space { space },
                ..
            }
            | Self::WithdrawAlert {
                space: Some(space), ..
            } => Some(Target::Space(space)),
            // It marks the caller's own agent's turn, whose alert goes out
            // once the turn ends.
            Self::AlertInChat { .. } => None,
            Self::ListAlerts { .. }
            | Self::SendAlert { .. }
            | Self::WithdrawAlert { space: None, .. }
            | Self::MarkAlertsRead { .. }
            | Self::ClearAlerts
            | Self::WatchAlerts { .. } => None,
        }
    }
}
