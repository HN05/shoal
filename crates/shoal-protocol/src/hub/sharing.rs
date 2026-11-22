//! What joined machines share through the hub, item by item, each kind of
//! item under keys of its own: a machine sends the hub its edits; the hub
//! keeps the last edit of each item that reached it, under a revision that
//! orders its copies, and sends it to every machine.
use serde::{Deserialize, Serialize};

use super::{shared_config, shared_repositories};

/// A machine's edit of a shared item: its new value, none for an item it
/// removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit<K, V> {
    pub key: K,
    pub value: Option<V>,
}

/// The hub's copy of a shared item: the last edit of it that reached the
/// hub, at the revision the hub gave it, later edits of any item having
/// higher ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copy<K, V> {
    pub key: K,
    pub value: Option<V>,
    pub revision: i64,
}

/// A machine's edits of one kind of shared item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edits {
    Files(Vec<shared_config::Edit>),
    Repositories(Vec<shared_repositories::Edit>),
}

/// The hub's copies of one kind of shared item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Copies {
    Files(Vec<shared_config::Copy>),
    Repositories(Vec<shared_repositories::Copy>),
}
