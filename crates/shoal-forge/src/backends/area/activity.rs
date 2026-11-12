//! What each forge does its own way with item activity, PR status and item
//! views.
use anyhow::Result;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    items::link::ItemKind,
    numbers::{IssueNumber, PullNumber},
    review::{
        status::PrStatus,
        view::{Details, Review},
    },
    updates::Snapshot,
};

/// How the forge reads an item's activity, a PR's status and an item's
/// view.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Activity: Backend {
    /// An issue's state and discussion.
    async fn issue_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: IssueNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, discussion, reviews, mergeability and checks.
    async fn pr_activity(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
    ) -> Result<Snapshot>;

    /// A PR's state, mergeability, checks and review state.
    async fn pr_status(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        number: PullNumber,
        url: &str,
    ) -> Result<PrStatus>;

    /// The item's content for a view, with its comments when `comments` is
    /// set and the forge reads them with the item.
    async fn view_details(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        comments: bool,
    ) -> Result<Details>;

    /// Fills the comments `details` lacks and returns a PR's reviews,
    /// recording in `errors` the parts it could not read.
    async fn view_discussion(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
        number: u64,
        details: &mut Details,
        errors: &mut Vec<String>,
    ) -> Result<Option<Vec<Review>>>;
}
