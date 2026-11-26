//! Changing a space's issues and PRs on its forge, and waiting for their
//! activity.
use anyhow::Result;
use tokio::time::Instant;

use crate::{
    connection::{Daemon, Prepared, Sendable as _, await_reply, request},
    forge::items::{
        action::Action,
        create::{NewIssue, PullOptions},
        item::Item,
        link::{ItemKind, LinkKind, Selection},
        reference::ItemInput,
    },
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    links::{OpenOutcome, Updates},
    model::Relation,
    protocol::{Body, ItemMethod, RepoTarget},
};

/// Link the `kind` item `item` names to `space` as `relation`, with the
/// title lists show when the caller knows it.
pub fn link(
    space: SpaceSelector,
    relation: Relation,
    kind: LinkKind,
    item: ItemInput,
    title: Option<String>,
) -> Prepared<shoal_protocol::links::Linked> {
    Prepared::new(ItemMethod::LinkItem {
        space,
        kind,
        item,
        relation,
        title,
    })
}

/// Link the issues, opened and related items `links` carries to `space`,
/// as a moved space brings them; the caller links its PRs.
pub async fn relink(
    daemon: &impl Daemon,
    space: SpaceSelector,
    links: shoal_protocol::model::SpaceLinks,
) -> Result<shoal_protocol::links::Linked> {
    request(daemon, ItemMethod::RelinkSpace { space, links }).await
}

/// Unlink `space`'s items of `relation`, only those of `kind` or the one
/// `item` names when given.
pub fn unlink(
    space: SpaceSelector,
    relation: Relation,
    kind: Option<LinkKind>,
    item: Option<ItemInput>,
) -> Prepared<shoal_protocol::links::Unlinked> {
    Prepared::new(ItemMethod::UnlinkItems {
        space,
        relation,
        kind,
        item,
    })
}

/// What [`await_updates`] asks, waiting up to `timeout_secs`, as the picker
/// that chooses the space whose items it watches judges it.
pub fn watch(
    space: SpaceSelector,
    selection: Selection,
    repository: Option<RepositoryId>,
    timeout_secs: Option<u64>,
) -> Prepared<Updates> {
    Prepared::new(ItemMethod::WatchItems {
        space,
        selection,
        repository,
        timeout_secs,
    })
}

/// The next activity on the items of `space` that `selection` selects,
/// waiting across daemon restarts until `deadline`. A chat without a
/// repository selects an item of the registered `repository`. `restarted`
/// says the daemon restarted before the wait began. Waiting acknowledges
/// nothing.
pub async fn await_updates(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    selection: Selection,
    repository: Option<RepositoryId>,
    deadline: Option<Instant>,
    restarted: bool,
) -> Result<Updates> {
    let method = || {
        let timeout = seconds_left(deadline);
        watch(
            space.clone(),
            selection.clone(),
            repository.clone(),
            timeout,
        )
        .into_operation()
    };
    let reply = await_reply(daemon, method, restarted);
    let body = match deadline {
        None => reply.await?,
        Some(deadline) => match tokio::time::timeout_at(deadline, reply).await {
            Ok(reply) => reply?,
            Err(_) => Body::PrUpdates(Updates {
                updates: Vec::new(),
                alerts: Vec::new(),
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
pub fn open_pull(
    workspace: SpaceSelector,
    options: PullOptions,
    push_held: bool,
) -> Prepared<OpenOutcome> {
    Prepared::new(ItemMethod::OpenPull {
        workspace,
        options,
        push_held,
    })
}

/// Open `issue` in the `target` repository, linking it to the space when
/// `link`.
pub fn open_issue(target: RepoTarget, issue: NewIssue, link: bool) -> Prepared<OpenOutcome> {
    Prepared::new(ItemMethod::OpenIssue {
        target,
        issue,
        link,
    })
}

/// Have the forge open a PR that reverts the merged PR `item`, by default
/// `space`'s linked one.
pub fn revert_pull(
    space: SpaceSelector,
    item: Option<ItemInput>,
    options: PullOptions,
) -> Prepared<OpenOutcome> {
    Prepared::new(ItemMethod::RevertPull {
        space,
        item,
        options,
    })
}

/// Carry out `action` on `item` in the `target` repository, or on the
/// space's linked item of `kind`.
pub fn act(
    target: RepoTarget,
    kind: ItemKind,
    item: Option<ItemInput>,
    action: Action,
) -> Prepared<Item> {
    Prepared::new(ItemMethod::ItemAction {
        target,
        kind,
        item,
        action,
        confirmed: None,
    })
}

/// Carry out `action`, a merge of the PR `item` names or of the linked
/// one, which the daemon refuses unless the merge still has the warning
/// `confirmed` names; without it, the merge goes ahead whatever it warns
/// about.
pub async fn merge(
    daemon: &impl Daemon,
    target: RepoTarget,
    item: Option<ItemInput>,
    action: Action,
    confirmed: Option<shoal_protocol::model::ConfirmedWarning>,
) -> Result<Item> {
    let method = ItemMethod::ItemAction {
        target,
        kind: ItemKind::Pr,
        item,
        action,
        confirmed,
    };
    request(daemon, method).await
}

#[cfg(test)]
mod tests;
