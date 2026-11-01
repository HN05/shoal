//! What `shoal view` prints.
use serde::{Deserialize, Serialize};

use crate::{
    client::{output::outputs, reviews::Review},
    forge::{ci::run_view::RunView, items::link::LinkKind, review::view::ItemView},
};

/// What `shoal view` shows of one selected item.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum Shown {
    Item(Box<ItemView>),
    Run(ShownRun),
    Review(ShownReview),
}

/// The workspace's latest local review.
#[derive(Serialize, Deserialize)]
pub struct ShownReview {
    pub kind: String,
    #[serde(flatten)]
    pub review: Box<Review>,
}

/// A linked CI run, as `view ci` shows it, or why it could not be read.
#[derive(Serialize, Deserialize)]
pub struct ShownRun {
    pub kind: LinkKind,
    #[serde(flatten)]
    pub read: RunRead,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum RunRead {
    Read(RunView),
    Failed { url: String, errors: Vec<String> },
}

outputs!(Shown);
