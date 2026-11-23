//! The alerts a hub's machines keep. Their numbers come from one sequence
//! across them, which the hub holds and never gives twice, so a stale number
//! never names another alert: the hub keeps spare numbers for each machine,
//! which numbers its new alerts from them without asking, and once none is
//! left, as while it cannot reach the hub, above the highest number it
//! knows; the hub gives a new number to an alert whose number another
//! machine took first. The user's alerts show on every machine: each
//! machine reports those it keeps, the hub keeps a copy of every machine's,
//! and each member follows the other machines' copies.
use serde::{Deserialize, Serialize};
use shoal_ids::member_ids::MemberId;

use crate::alerts::Alert;

/// What a machine reports of the alerts it keeps, which the hub answers
/// with an [`AlertNumbering`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertsReport {
    /// The spares the hub kept for it that it has not used, smallest
    /// first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spares: Vec<i64>,
    /// The numbers its alerts took that the hub did not give it: those
    /// above the highest it knew, and every one it had before the hub
    /// numbered its alerts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unconfirmed: Vec<i64>,
    /// The user's alerts it keeps, as they changed since its last report;
    /// none when they did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub users: Option<UsersAlerts>,
}

/// What a machine reports of the user's alerts it keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsersAlerts {
    /// Every one, which replaces what the hub had for it, as a machine
    /// sends once its link opens.
    Whole(Vec<Alert>),
    /// What changed since its last report.
    Changes {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<Alert>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<i64>,
    },
}

/// What the hub writes on the stream a member follows the other machines'
/// alerts to the user on, one JSON line each; every alert names the machine
/// that keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertCopies {
    /// Every other machine's, which replace what the member had, as the
    /// stream opens.
    Whole(Vec<Alert>),
    /// The alerts of `machine` that changed, and those it no longer keeps.
    Changed {
        machine: MemberId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changed: Vec<Alert>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<i64>,
    },
    /// The hub revoked `machine`, or it left, so its alerts go.
    Dropped { machine: MemberId },
}

/// The alert numbers the hub keeps for a machine, which it answers each of
/// the machine's reports of its alerts with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertNumbering {
    /// The numbers the machine gives its new alerts, smallest first.
    pub spares: Vec<i64>,
    /// The reported numbers no other machine took, which stay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed: Vec<i64>,
    /// The reported numbers another machine took first, each with the
    /// number its alert takes instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renumbered: Vec<AlertRenumbering>,
    /// What other machines did to its alerts to the user that it has not
    /// reported doing yet.
    #[serde(default, skip_serializing_if = "AlertMarks::is_empty")]
    pub marked: AlertMarks,
}

/// What a machine did to another machine's alerts to the user, which the
/// hub passes on to the machine that keeps them: those it read, and those
/// it cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertMarks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<i64>,
}

impl AlertMarks {
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.deleted.is_empty()
    }
}

/// The number `to` an alert takes instead of `from`, which another
/// machine's alert took first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertRenumbering {
    pub from: i64,
    pub to: i64,
}
