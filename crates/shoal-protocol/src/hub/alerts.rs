//! The alerts a hub's machines keep, numbered from one sequence across
//! them, which the hub holds and never gives twice, so a stale number never
//! names another alert. The hub keeps spare numbers for each machine, which
//! numbers its new alerts from them without asking, and once none is left,
//! as while it cannot reach the hub, above the highest number it knows; the
//! hub gives a new number to an alert whose number another machine took
//! first.
use serde::{Deserialize, Serialize};

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
}

/// The number `to` an alert takes instead of `from`, which another
/// machine's alert took first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertRenumbering {
    pub from: i64,
    pub to: i64,
}
