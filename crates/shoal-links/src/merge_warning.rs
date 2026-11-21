//! What merging a PR warns about: the sensitive files that the last
//! conflict check of the merged head found changed on both sides. It reads
//! that record and checks nothing again, so merging stays as fast without
//! one. A branch's local merge reads its own record the same way.
use anyhow::{Context, Result, ensure};
use shoal_daemon_base::callers::Caller;
use shoal_forge::{RepoRequests, items::action::ActionRequests};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        action::Action,
        item::Item,
        link::ItemKind,
        reference::{ItemInput, ItemRef},
    },
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    model::{ChangedOnBothSides, ConfirmedWarning, MergeWarning},
    protocol::RepoTarget,
};
use shoal_refs::CommitId;

use crate::{Links, actions::configure_merge, daemon::Daemon, place::Access};

impl Links {
    /// The warning for merging the PR `input` names in the `target`
    /// repository, or the space's linked PR, as the workspaces that link it
    /// last checked its head against its base branch.
    pub async fn merge_warning(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: Option<ItemInput>,
    ) -> Result<Option<MergeWarning>> {
        let pull = self.context_pull(daemon, target, input, Access::Read, None);
        let (forge, url) = pull.await?;
        let number = url.pull()?;
        let recorded = self.recorded_for_pull(url.clone()).await?;
        if recorded.is_empty() {
            return Ok(None);
        }
        let account = self.user_account();
        let current = forge.read(&account, ItemKind::Pr, number.get()).await?;
        let head = current.head_sha.context("the forge names no PR head")?;
        let base = current.item.pr.context("the forge names no PR base")?.base;
        let merged = Merged {
            head,
            target: base.into(),
        };
        let warning = warning(format!("PR #{number}"), recorded, &merged);
        Ok(warning.map(|warning| MergeWarning {
            url: Some(url),
            ..warning
        }))
    }

    /// Carry out `action`, a merge of the PR `input` names or of the
    /// space's linked one, unless its warning is no longer the one the user
    /// `confirmed`. The PR is resolved once, so a link that changes while
    /// the warning is read leaves the merge with the PR it checked.
    pub async fn confirmed_merge(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: Option<ItemInput>,
        action: Action,
        confirmed: &ConfirmedWarning,
        caller: Option<&Caller>,
    ) -> Result<Item> {
        ensure!(action.merges(), "only a merge confirms a merge warning");
        let pull = self.context_pull(daemon, target, input, Access::Read, None);
        let (_, url) = pull.await?;
        let current = (self.merge_warning(daemon, target, Some(url.clone().into()))).await?;
        ensure!(
            current == confirmed.warning,
            "the merge warning changed since you confirmed the merge; run shoal merge again"
        );
        let input = Some(url.into());
        (self.item_action(daemon, target, ItemKind::Pr, input, action, caller)).await
    }

    /// Bind a merge in `action` of the PR `input` names, or of `target`'s
    /// linked PR, for a scoped `caller`, to the commit the PR's head has now, unless it names one,
    /// and to the `[merge] method` it merges by, unless it names one, and
    /// return what it does as an approval of it binds it: a later commit,
    /// another method or another message needs another decision, and the
    /// forge refuses to merge another commit.
    pub async fn bind_merge(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: Option<ItemInput>,
        action: &mut Action,
        caller: &Caller,
    ) -> Result<String> {
        let item = self.target_item(
            daemon,
            target,
            input,
            ItemKind::Pr,
            Access::Change,
            Some(caller),
        );
        let (target, url) = item.await?;
        configure_merge(daemon, target.own_place(), action).await?;
        if action.merges() && action.merge_head().is_none() {
            let forge = target.into_forge()?;
            let pull = forge.pull_request(&self.agent_account(), &url).await?;
            let head = pull.head_commit.context("the forge names no PR head")?;
            action.bind_merge_head(head);
        }
        let at = (action.merge_head()).map_or_else(String::new, |head| format!(" at {head}"));
        Ok(format!(
            "{} {url}{at}{}",
            action.name(),
            merge_terms(action)
        ))
    }

    /// The forge and the PR `input` names in the `target` repository, or
    /// the space's linked PR, refusing a scoped `caller` a change elsewhere
    /// than in its own repository.
    pub async fn context_pull(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        input: Option<ItemInput>,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<(ForgeRepo, ItemRef)> {
        let item = self.target_item(daemon, target, input, ItemKind::Pr, access, caller);
        let (target, pull) = item.await?;
        Ok((target.into_forge()?, pull))
    }

    /// The records of the workspaces that link the PR `url` which list
    /// files changed on both sides.
    pub async fn recorded_for_pull(&self, url: ItemRef) -> Result<Vec<ChangedOnBothSides>> {
        let records = self.records.clone();
        self.store
            .run(move |db| {
                let mut found = Vec::new();
                for (id, record) in records.changed_on_both_sides(db)? {
                    if links_pull(db, &id, &url)? {
                        found.push(record);
                    }
                }
                Ok(found)
            })
            .await
    }
}

/// The method and squash message `action` merges by, as an approval of it
/// names them, the whole message so another body needs another decision:
/// ` by squash as "Fix login\n\nWhy"`; nothing for the forge's default.
fn merge_terms(action: &Action) -> String {
    let (Action::Merge {
        method, message, ..
    }
    | Action::AutoMerge {
        method, message, ..
    }) = action
    else {
        return String::new();
    };
    let mut terms = method.map_or_else(String::new, |method| format!(" by {method}"));
    if let Some(message) = message {
        terms.push_str(&format!(" as {:?}", message.trim()));
    }
    terms
}

/// Whether the workspace `id` links the PR `url`.
fn links_pull(db: &rusqlite::Connection, id: &SpaceId, url: &ItemRef) -> Result<bool> {
    let links = crate::linked::assigned_items(db, id, Some(ItemKind::Pr))?;
    Ok(links.contains(url))
}

/// What a merge brings together: a head and the branch it merges into.
pub struct Merged {
    pub head: CommitId,
    pub target: String,
}

/// The warning for merging `subject` as `merged`, from the first of
/// `recorded` that checked that head against that branch.
pub fn warning(
    subject: String,
    recorded: Vec<ChangedOnBothSides>,
    merged: &Merged,
) -> Option<MergeWarning> {
    let recorded = recorded
        .into_iter()
        .find(|recorded| recorded.head == merged.head && recorded.target == merged.target)?;
    Some(MergeWarning {
        subject,
        target: recorded.target,
        files: recorded.files,
        url: None,
    })
}
