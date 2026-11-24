//! Issues and PRs the daemon reads for `shoal view` and apps, as the forge
//! reports them now, with the logs of failed checks.
use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_daemon_base::forge_access::ForgeAccess;
use shoal_forge::{
    activity::view::ViewRequests, ci::checks::CheckRequests, transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    activity::view::ItemView,
    items::{
        link::Selection,
        reference::{ItemInput, ItemRef},
    },
};
use shoal_protocol::{model::SelectedItem, protocol::RepoTarget, views::Viewed};

use crate::{Links, daemon::Daemon, place::Place, space::LinkSpace, target::ForgeTarget};

impl Links {
    /// The items `selection` names in the `target` repository, each issue
    /// and PR among them read as the account of `caller`, a scoped caller's
    /// agent account or the user's own logins, in selection order.
    pub async fn view_items(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        selection: &Selection,
        comments: bool,
        caller: Option<&Caller>,
    ) -> Result<Viewed> {
        let account = self.forges.read_account(caller);
        let target = (self.selection_target(daemon, target, selection, true, caller)).await?;
        let items = (self.view_selection(daemon, &target, selection, &account)).await?;
        // Nothing to read, so no forge is needed, as for a local repository.
        if !items.iter().any(|item| item.kind().item().is_some()) {
            return Ok(Viewed {
                items,
                views: Vec::new(),
            });
        }
        let forge = target.forge()?;
        let mut views = read_views(&self.forges, forge, &account, &items, comments).await;
        // Logs are read as the agent account, whose token is the reading
        // one's for a scoped caller and the user's without one of its own.
        let own_token =
            caller.is_none() && (self.config.current().machine.forges).has_agent_token(&forge.host);
        let agent = own_token.then(|| self.forges.agent_account());
        forge
            .read_failed_logs(agent.as_ref().unwrap_or(&account), &mut views)
            .await;
        Ok(Viewed { items, views })
    }

    /// The items `selection` names in `target`: in a space's own
    /// repository, where it may name linked ones and a number takes the
    /// kind of the linked item with it, otherwise the one its number or URL
    /// names.
    async fn view_selection(
        &self,
        daemon: &dyn Daemon,
        target: &ForgeTarget,
        selection: &Selection,
        account: &Account,
    ) -> Result<Vec<SelectedItem>> {
        let selection = match target.own_place().and_then(Place::space) {
            Some(space) => (self.classify_number(space, selection, account)).await?,
            None => selection.clone(),
        };
        (self.target_selection(daemon, target, &selection, true, account)).await
    }

    /// `selection` with the kind of a number selected without one: the kind
    /// of the item with that number linked to the space under any relation,
    /// otherwise the kind the forge reports for it.
    async fn classify_number(
        &self,
        space: &LinkSpace,
        selection: &Selection,
        account: &Account,
    ) -> Result<Selection> {
        let (None, Some(ItemInput::Number(number))) = (selection.kind, &selection.input) else {
            return Ok(selection.clone());
        };
        let kind = (self.space_number_kind(space, *number, account)).await?;
        Ok(Selection {
            kind: Some(kind),
            input: selection.input.clone(),
        })
    }
}

/// Each issue and PR among `items` as the forge reports it now, in
/// selection order; one that cannot be read records why.
async fn read_views(
    forges: &ForgeAccess,
    own: &ForgeRepo,
    account: &Account,
    items: &[SelectedItem],
    comments: bool,
) -> Vec<ItemView> {
    futures_util::future::join_all(items.iter().filter_map(|item| {
        let kind = item.kind().item()?;
        Some(async move {
            match viewed_item_forge(forges, own, &item.url).await {
                Ok(forge) => forge.view(account, kind, &item.url, comments).await,
                Err(error) => ItemView {
                    url: item.url.clone(),
                    kind,
                    details: None,
                    check_logs: Vec::new(),
                    errors: vec![format!("{error:#}")],
                },
            }
        })
    }))
    .await
}

/// The repository `item` is in: `own`, the one the view reads in, or the
/// other one its URL names.
async fn viewed_item_forge(
    forges: &ForgeAccess,
    own: &ForgeRepo,
    item: &ItemRef,
) -> Result<ForgeRepo> {
    if item.repository() == own {
        return Ok(own.clone());
    }
    forges.item_forge(item).await
}
