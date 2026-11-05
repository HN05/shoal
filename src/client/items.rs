//! Changing a space's issues and PRs on its forge: linking them, opening,
//! reverting and acting on them, and waiting for their activity.
use anyhow::Result;
use tokio::time::Instant;

use crate::{
    client::{
        connection::{Daemon, await_reply, request},
        links::{OpenOutcome, Updates, pr::Action as PrAction},
    },
    forge::items::{
        action::Action,
        create::{NewIssue, PullOptions},
        item::Item,
        link::{ItemKind, LinkKind, Selection},
    },
    model::{LinkedItem, Relation},
    protocol::{Body, ItemContext, ItemMethod},
};

/// Link `item` to `space` as `relation`.
pub async fn link(
    daemon: &impl Daemon,
    space: &str,
    relation: Relation,
    item: &LinkedItem,
) -> Result<()> {
    let method = ItemMethod::LinkItem {
        space: space.to_owned(),
        kind: item.kind,
        url: item.url.clone(),
        relation,
        title: item.title.clone(),
    };
    request(daemon, method).await
}

/// Unlink `space`'s items of `relation`, only those of `kind` or the one
/// `url` names when given.
pub async fn unlink(
    daemon: &impl Daemon,
    space: String,
    relation: Relation,
    kind: Option<LinkKind>,
    url: Option<String>,
) -> Result<()> {
    let method = ItemMethod::UnlinkItems {
        space,
        relation,
        kind,
        url,
    };
    request(daemon, method).await
}

/// Link and watch the PR `url` as the one `space` was opened on.
pub async fn watch_pr(daemon: &impl Daemon, space: String, url: String) -> Result<()> {
    let action = PrAction::Watch { url };
    request(daemon, ItemMethod::SetPr { space, action }).await
}

/// The next activity on the items of `space` that `selection` selects,
/// waiting across daemon restarts until `deadline`, when the updates come
/// back timed out; `restarted` says the daemon restarted before the wait
/// began. Waiting acknowledges nothing.
pub async fn await_updates(
    daemon: &impl Daemon,
    space: &str,
    selection: Selection,
    deadline: Option<Instant>,
    restarted: bool,
) -> Result<Updates> {
    let method = || ItemMethod::WatchItems {
        space: space.to_owned(),
        selection: selection.clone(),
        timeout_secs: seconds_left(deadline),
    };
    let reply = await_reply(daemon, method, restarted);
    let body = match deadline {
        None => reply.await?,
        Some(deadline) => match tokio::time::timeout_at(deadline, reply).await {
            Ok(reply) => reply?,
            Err(_) => Body::PrUpdates(Updates {
                updates: Vec::new(),
                messages: Vec::new(),
                leftovers: Vec::new(),
                timed_out: true,
                superseded: false,
            }),
        },
    };
    Updates::try_from(body)
}

/// The whole seconds before `deadline`, at least one.
pub fn seconds_left(deadline: Option<Instant>) -> Option<u64> {
    let left = deadline?.saturating_duration_since(Instant::now());
    Some((left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1))
}

/// Open the PR of `workspace`'s branch with `options`; `push_held` says
/// `[permissions] push` held the branch's push back for approval.
pub async fn open_pull(
    daemon: &impl Daemon,
    workspace: String,
    options: PullOptions,
    push_held: bool,
) -> Result<OpenOutcome> {
    let method = ItemMethod::OpenPull {
        workspace,
        options,
        push_held,
    };
    request(daemon, method).await
}

/// Open `issue` in `context`'s repository, or in the one `repository`
/// names, linking it to the space when `link`.
pub async fn open_issue(
    daemon: &impl Daemon,
    context: ItemContext,
    issue: NewIssue,
    repository: Option<String>,
    link: bool,
) -> Result<OpenOutcome> {
    let method = ItemMethod::OpenIssue {
        context,
        issue,
        repository,
        link,
    };
    request(daemon, method).await
}

/// Have the forge open a PR that reverts the merged PR `item`, by default
/// `space`'s linked one.
pub async fn revert_pull(
    daemon: &impl Daemon,
    space: String,
    item: Option<String>,
    options: PullOptions,
) -> Result<OpenOutcome> {
    let method = ItemMethod::RevertPull {
        space,
        item,
        options,
    };
    request(daemon, method).await
}

/// Carry out `action` on `item` in `context`, or on the space's linked item
/// of `kind`.
pub async fn act(
    daemon: &impl Daemon,
    context: ItemContext,
    kind: ItemKind,
    item: Option<String>,
    action: Action,
) -> Result<Item> {
    let method = ItemMethod::ItemAction {
        context,
        kind,
        item,
        action,
    };
    request(daemon, method).await
}

#[cfg(test)]
mod tests;
