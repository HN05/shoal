//! What a space or a registered checkout reads from its forge through the
//! daemon, which chooses the account: the agent account for a workspace
//! process or a chat's agent, the user's own logins otherwise.
use crate::forge::items::reference::ItemInput;
use anyhow::{Context, Result};

use crate::{
    connection::{Daemon, joined, pages, request},
    forge::{
        PullRequest,
        capability::Capabilities,
        conventions::labels::Label,
        download::Download,
        items::{
            issue::IssueDetails,
            item::Item,
            link::{ItemKind, LinkKind, Selection},
            list::Search,
            reaction::allowed::AllowedReactions,
            revert::MergedPull,
        },
        numbers::CommentId,
        raw::RawRequest,
        review::{reviewers::Reviewer, thread::Thread},
    },
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::{Blocker, MergeWarning, Selected},
    protocol::{DownloadPiece, ItemContext, ItemMethod, RepoTarget},
};

/// The forge's JSON response to `request` in the `target` repository,
/// reassembled from the pieces the daemon sends it in.
pub async fn forge_api(
    daemon: &impl Daemon,
    target: RepoTarget,
    request: RawRequest,
) -> Result<serde_json::Value> {
    let method = ItemMethod::ForgeApi { target, request };
    let pieces = pages::<String>(daemon, method).await?;
    serde_json::from_str(&pieces.concat()).context("the daemon sent invalid JSON")
}

/// The file at `url` on the forge of the `target` repository, reassembled
/// from the pieces the daemon sends it in.
pub async fn forge_download(
    daemon: &impl Daemon,
    target: RepoTarget,
    url: String,
) -> Result<Download> {
    let method = ItemMethod::ForgeDownload { target, url };
    let pieces = pages(daemon, method).await?;
    DownloadPiece::join(pieces)
}

/// What the forge of the `target` repository supports.
pub async fn capabilities(daemon: &impl Daemon, target: RepoTarget) -> Result<Capabilities> {
    request(daemon, ItemMethod::Capabilities { target }).await
}

/// The reactions the forge of the `target` repository allows.
pub async fn reactions(daemon: &impl Daemon, target: RepoTarget) -> Result<AllowedReactions> {
    request(daemon, ItemMethod::Reactions { target }).await
}

/// The labels of the `target` repository.
pub async fn labels(daemon: &impl Daemon, target: RepoTarget) -> Result<Vec<Label>> {
    pages(daemon, ItemMethod::Labels { target }).await
}

/// Issues or PRs of the `target` repository that `search` matches, newest
/// first.
pub async fn list_items(
    daemon: &impl Daemon,
    target: RepoTarget,
    kind: ItemKind,
    search: Search,
) -> Result<Vec<Item>> {
    let method = ItemMethod::ListItems {
        target,
        kind,
        search,
    };
    pages(daemon, method).await
}

/// The items `selection` names in the `target` repository: in a space's
/// own, where it may name linked ones, or elsewhere, where only a number or
/// URL selects one. With `elsewhere`, an issue, PR or CI run URL of another
/// repository selects that item.
pub async fn select_items(
    daemon: &impl Daemon,
    target: RepoTarget,
    selection: Selection,
    elsewhere: bool,
) -> Result<Selected> {
    let method = ItemMethod::SelectItems {
        target,
        selection,
        elsewhere,
    };
    request(daemon, method).await
}

/// The kind of the item numbered `number` in the `target` repository: in a
/// space's own, that of the linked item with that number, otherwise the
/// kind the forge reports.
pub async fn number_kind(
    daemon: &impl Daemon,
    target: RepoTarget,
    number: u64,
) -> Result<LinkKind> {
    request(daemon, ItemMethod::NumberKind { target, number }).await
}

/// The open issue `item` names in `context`'s repository, by number or
/// URL, reassembled from the pieces of JSON the daemon sends it in.
pub async fn issue_details(
    daemon: &impl Daemon,
    context: ItemContext,
    item: ItemInput,
) -> Result<IssueDetails> {
    let method = ItemMethod::IssueDetails {
        context,
        item,
        closed: false,
    };
    joined(daemon, method, "issue").await
}

/// [`issue_details`] of an issue in any state, such as the one a workspace
/// already has.
pub async fn linked_issue_details(
    daemon: &impl Daemon,
    context: ItemContext,
    item: ItemInput,
) -> Result<IssueDetails> {
    let method = ItemMethod::IssueDetails {
        context,
        item,
        closed: true,
    };
    joined(daemon, method, "issue").await
}

/// The open issues the forge records as blocking the issue `item` names in
/// `context`'s repository.
pub async fn issue_blockers(
    daemon: &impl Daemon,
    context: ItemContext,
    item: ItemInput,
) -> Result<Vec<Blocker>> {
    pages(daemon, ItemMethod::IssueBlockers { context, item }).await
}

/// The PR `item` names in `context`'s repository, with where its head is.
pub async fn pull_request(
    daemon: &impl Daemon,
    context: ItemContext,
    item: ItemInput,
) -> Result<PullRequest> {
    request(daemon, ItemMethod::PullRequest { context, item }).await
}

/// Who the PR `item` names in the `target` repository, or the space's
/// linked PR, can ask for a review.
pub async fn reviewers(
    daemon: &impl Daemon,
    target: RepoTarget,
    item: Option<ItemInput>,
) -> Result<Vec<Reviewer>> {
    pages(daemon, ItemMethod::Reviewers { target, item }).await
}

/// The review thread of `comment` on the PR `item` names in the `target`
/// repository, or on the space's linked PR.
pub async fn review_thread(
    daemon: &impl Daemon,
    target: RepoTarget,
    item: Option<ItemInput>,
    comment: CommentId,
) -> Result<Thread> {
    let method = ItemMethod::ReviewThread {
        target,
        item,
        comment,
    };
    request(daemon, method).await
}

/// The sensitive files merging the PR `item`, by default the linked one,
/// brings to a target that changed them too, for the caller to confirm;
/// `None` when it brings none.
pub async fn merge_warning(
    daemon: &impl Daemon,
    target: RepoTarget,
    item: Option<ItemInput>,
) -> Result<Option<MergeWarning>> {
    request(daemon, ItemMethod::MergeWarning { target, item }).await
}

/// The merged PR `item`, by default `space`'s linked one, with what
/// reverting its merge needs.
pub async fn merged_pull(
    daemon: &impl Daemon,
    space: SpaceSelector,
    item: Option<ItemInput>,
) -> Result<MergedPull> {
    request(daemon, ItemMethod::MergedPull { space, item }).await
}
