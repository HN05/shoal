//! What `shoal view` reads through the daemon: issues and PRs as the forge
//! reports them, with failed checks' logs, and CI runs with their jobs and
//! failed jobs' logs, in the order they were selected.
use serde::{Deserialize, Serialize};

use crate::{forge::review::view::ItemView, model::SelectedItem};

/// The items a selection named, in order, and the views of the issues and
/// PRs among them, in the same order.
#[derive(Serialize, Deserialize)]
pub struct Viewed {
    pub items: Vec<SelectedItem>,
    pub views: Vec<ItemView>,
}
