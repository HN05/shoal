//! What `shoal view` reads through the daemon: the items a selection named,
//! with the views of its issues and PRs.
use serde::{Deserialize, Serialize};

use crate::{forge::review::view::ItemView, model::SelectedItem};

/// The items a selection named, in order, and the views of the issues and
/// PRs among them, in the same order.
#[derive(Serialize, Deserialize)]
pub struct Viewed {
    pub items: Vec<SelectedItem>,
    pub views: Vec<ItemView>,
}
