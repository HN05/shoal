//! What a space or a registered checkout reads from its forge through the
//! daemon, which chooses the account: the agent account for a workspace
//! process or a chat's agent, the user's own logins otherwise.
use anyhow::{Context, Result};

use crate::space_names::SpaceSelector;
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
    model::{Dependency, MergeWarning, Selected},
    protocol::{DownloadPiece, ItemContext, ItemMethod},
};

/// The forge's JSON response to `request` in `context`'s repository, or in
/// the one `repository` names, reassembled from the pieces the daemon sends
/// it in.
pub async fn forge_api(
    daemon: &impl Daemon,
    context: ItemContext,
    request: RawRequest,
    repository: Option<String>,
) -> Result<serde_json::Value> {
    let method = ItemMethod::ForgeApi {
        context,
        request,
        repository,
    };
    let pieces = pages::<String>(daemon, method).await?;
    serde_json::from_str(&pieces.concat()).context("the daemon sent invalid JSON")
}

/// The file at `url` on the forge of `context`'s repository, or of the one
/// `repository` names, reassembled from the pieces the daemon sends it in.
pub async fn forge_download(
    daemon: &impl Daemon,
    context: ItemContext,
    url: String,
    repository: Option<String>,
) -> Result<Download> {
    let method = ItemMethod::ForgeDownload {
        context,
        url,
        repository,
    };
    let pieces = pages(daemon, method).await?;
    DownloadPiece::join(pieces)
}

/// What the forge of `context`'s repository, or of the one `repository`
/// names, supports.
pub async fn capabilities(
    daemon: &impl Daemon,
    context: ItemContext,
    repository: Option<String>,
) -> Result<Capabilities> {
    let method = ItemMethod::Capabilities {
        context,
        repository,
    };
    request(daemon, method).await
}

/// The reactions the forge of `context`'s repository, or of the one
/// `repository` names, allows.
pub async fn reactions(
    daemon: &impl Daemon,
    context: ItemContext,
    repository: Option<String>,
) -> Result<AllowedReactions> {
    let method = ItemMethod::Reactions {
        context,
        repository,
    };
    request(daemon, method).await
}

/// The labels of `context`'s repository, or of the one `repository` names.
pub async fn labels(
    daemon: &impl Daemon,
    context: ItemContext,
    repository: Option<String>,
) -> Result<Vec<Label>> {
    let method = ItemMethod::Labels {
        context,
        repository,
    };
    pages(daemon, method).await
}

/// Issues or PRs of `context`'s repository, or of the one `repository`
/// names, that `search` matches, newest first.
pub async fn list_items(
    daemon: &impl Daemon,
    context: ItemContext,
    kind: ItemKind,
    search: Search,
    repository: Option<String>,
) -> Result<Vec<Item>> {
    let method = ItemMethod::ListItems {
        context,
        kind,
        search,
        repository,
    };
    pages(daemon, method).await
}

/// The items `selection` names in `context`: in a space, where it may name
/// linked ones, or in a registered checkout, where only a number or URL
/// selects one. With `elsewhere`, an issue, PR or CI run URL of another
/// repository selects that item.
pub async fn select_items(
    daemon: &impl Daemon,
    context: ItemContext,
    selection: Selection,
    elsewhere: bool,
) -> Result<Selected> {
    let method = ItemMethod::SelectItems {
        context,
        selection,
        elsewhere,
    };
    request(daemon, method).await
}

/// The kind of the item numbered `number` in `context`: in a space, that of
/// the linked item with that number, otherwise the kind the forge reports.
pub async fn number_kind(
    daemon: &impl Daemon,
    context: ItemContext,
    number: u64,
) -> Result<LinkKind> {
    request(daemon, ItemMethod::NumberKind { context, number }).await
}

/// The issue `item` names in `context`'s repository, by number or URL,
/// reassembled from the pieces of JSON the daemon sends it in.
pub async fn issue_details(
    daemon: &impl Daemon,
    context: ItemContext,
    item: String,
) -> Result<IssueDetails> {
    joined(daemon, ItemMethod::IssueDetails { context, item }, "issue").await
}

/// The open issues the forge records as blocking the issue `item` names in
/// `context`'s repository.
pub async fn issue_blockers(
    daemon: &impl Daemon,
    context: ItemContext,
    item: String,
) -> Result<Vec<Dependency>> {
    pages(daemon, ItemMethod::IssueBlockers { context, item }).await
}

/// The PR `item` names in `context`'s repository, with where its head is.
pub async fn pull_request(
    daemon: &impl Daemon,
    context: ItemContext,
    item: String,
) -> Result<PullRequest> {
    request(daemon, ItemMethod::PullRequest { context, item }).await
}

/// Who the PR `item` names in `context`'s repository, or the space's linked
/// PR, can ask for a review.
pub async fn reviewers(
    daemon: &impl Daemon,
    context: ItemContext,
    item: Option<String>,
) -> Result<Vec<Reviewer>> {
    pages(daemon, ItemMethod::Reviewers { context, item }).await
}

/// The review thread of `comment` on the PR `item` names in `context`'s
/// repository, or on the space's linked PR.
pub async fn review_thread(
    daemon: &impl Daemon,
    context: ItemContext,
    item: Option<String>,
    comment: CommentId,
) -> Result<Thread> {
    let method = ItemMethod::ReviewThread {
        context,
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
    context: ItemContext,
    item: Option<String>,
) -> Result<Option<MergeWarning>> {
    request(daemon, ItemMethod::MergeWarning { context, item }).await
}

/// The merged PR `item`, by default `space`'s linked one, with what
/// reverting its merge needs.
pub async fn merged_pull(
    daemon: &impl Daemon,
    space: SpaceSelector,
    item: Option<String>,
) -> Result<MergedPull> {
    request(daemon, ItemMethod::MergedPull { space, item }).await
}
