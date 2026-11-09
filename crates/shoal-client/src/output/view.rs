//! What `shoal view` prints.
use serde::{Deserialize, Serialize};

use crate::{
    forge::{ci::run_view::RunView, items::link::LinkKind, review::view::ItemView},
    model::Relation,
    output::outputs,
    reviews::Review,
};

/// What `shoal view` shows of one selected item.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum Shown {
    Item(ShownItem),
    Run(ShownRun),
    Review(ShownReview),
}

/// An issue or PR as the forge reports it, with the relations under which
/// the space links it.
#[derive(Serialize, Deserialize)]
pub struct ShownItem {
    #[serde(flatten)]
    pub view: Box<ItemView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<Relation>,
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
