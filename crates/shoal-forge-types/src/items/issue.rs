//! An issue's details.

use serde::{Deserialize, Serialize};

use crate::{items::reference::ItemRef, numbers::IssueNumber};

/// An issue's title, description, labels and how many comments it has,
/// where the forge says, with the URL of its number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDetails {
    pub number: IssueNumber,
    pub url: ItemRef,
    pub title: String,
    pub details: String,
    pub labels: Vec<String>,
    pub comments: Option<u64>,
}
