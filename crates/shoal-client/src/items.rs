//! Changing a space's issues and PRs on its forge: linking them, opening,
//! reverting and acting on them, and waiting for their activity.
use anyhow::Result;
use tokio::time::Instant;

use crate::repository_ids::RepositoryId;
use crate::space_names::SpaceSelector;
use crate::{
    connection::{Daemon, await_reply, request},
    forge::items::{
        action::Action,
        create::{NewIssue, PullOptions},
        item::Item,
        link::{ItemKind, LinkKind, Selection},
        reference::ItemInput,
    },
    links::{OpenOutcome, Updates},
    model::Relation,
    protocol::{Body, ItemContext, ItemMethod},
};

/// Link the `kind` item `item` names to `space` as `relation`, with the
/// title lists show when the caller knows it.
pub async fn link(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    relation: Relation,
    kind: LinkKind,
    item: ItemInput,
    title: Option<String>,
) -> Result<crate::links::Linked> {
    let method = ItemMethod::LinkItem {
        space: space.clone(),
        kind,
        item,
        relation,
        title,
    };
    request(daemon, method).await
}

/// Link the issues, opened and related items `links` carries to `space`,
/// as a moved space brings them; the caller links its PRs.
pub async fn relink(
    daemon: &impl Daemon,
    space: SpaceSelector,
    links: crate::model::SpaceLinks,
) -> Result<crate::links::Linked> {
    request(daemon, ItemMethod::RelinkSpace { space, links }).await
}

/// Unlink `space`'s items of `relation`, only those of `kind` or the one
/// `item` names when given.
pub async fn unlink(
    daemon: &impl Daemon,
    space: SpaceSelector,
    relation: Relation,
    kind: Option<LinkKind>,
    item: Option<ItemInput>,
) -> Result<crate::links::Unlinked> {
    let method = ItemMethod::UnlinkItems {
        space,
        relation,
        kind,
        item,
    };
    request(daemon, method).await
}

/// The next activity on the items of `space` that `selection` selects, an
/// item of the registered `repository` for a chat without a repository,
/// waiting across daemon restarts until `deadline`, when the updates come
/// back timed out; `restarted` says the daemon restarted before the wait
/// began. Waiting acknowledges nothing.
pub async fn await_updates(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    selection: Selection,
    repository: Option<RepositoryId>,
    deadline: Option<Instant>,
    restarted: bool,
) -> Result<Updates> {
    let method = || ItemMethod::WatchItems {
        space: space.clone(),
        selection: selection.clone(),
        repository: repository.clone(),
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
/// the `push` permission held the branch's push back for approval.
pub async fn open_pull(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
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
    space: SpaceSelector,
    item: Option<ItemInput>,
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
    item: Option<ItemInput>,
    action: Action,
) -> Result<Item> {
    let method = ItemMethod::ItemAction {
        context,
        kind,
        item,
        action,
        confirmed: None,
    };
    request(daemon, method).await
}

/// Carry out `action`, a merge of the PR `item` names or of the linked
/// one, which the daemon refuses unless the merge still has the warning
/// `confirmed` names; without it, the merge goes ahead whatever it warns
/// about.
pub async fn merge(
    daemon: &impl Daemon,
    context: ItemContext,
    item: Option<ItemInput>,
    action: Action,
    confirmed: Option<crate::model::ConfirmedWarning>,
) -> Result<Item> {
    let method = ItemMethod::ItemAction {
        context,
        kind: ItemKind::Pr,
        item,
        action,
        confirmed,
    };
    request(daemon, method).await
}

#[cfg(test)]
mod tests;
