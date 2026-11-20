//! Issues and PRs the daemon reads for `shoal view` and apps, as the forge
//! reports them now, with the logs of failed checks.
use anyhow::Result;
use shoal_links::place::Place;
use shoal_links::space::LinkSpace;
use shoal_links::target::ForgeTarget;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::{
        ForgeRepo,
        items::{
            link::Selection,
            reference::{ItemInput, ItemRef},
        },
        review::view::ItemView,
    },
    model::SelectedItem,
    protocol::RepoTarget,
};
use shoal_forge::{account::Account, ci::checks::CheckRequests, review::view::ViewRequests};
use shoal_protocol::views::Viewed;

impl Manager {
    /// The items `selection` names in the `target` repository, each issue
    /// and PR among them read as the account of `caller`, a scoped caller's
    /// agent account or the user's own logins, in selection order.
    pub async fn view_items(
        &self,
        target: &RepoTarget,
        selection: &Selection,
        comments: bool,
        caller: Option<&Caller>,
    ) -> Result<Viewed> {
        let account = self.links.read_account(caller);
        let target = self
            .links
            .selection_target(self, target, selection, true, caller)
            .await?;
        let items = self.view_selection(&target, selection, &account).await?;
        // Nothing to read, so no forge is needed, as for a local repository.
        if !items.iter().any(|item| item.kind().item().is_some()) {
            return Ok(Viewed {
                items,
                views: Vec::new(),
            });
        }
        let forge = target.forge()?;
        let mut views = self.read_views(forge, &account, &items, comments).await;
        // Logs are read as the agent account, whose token is the reading
        // one's for a scoped caller and the user's without one of its own.
        let own_token =
            caller.is_none() && self.config().machine.forges.has_agent_token(&forge.host);
        let agent = own_token.then(|| self.links.agent_account());
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
        target: &ForgeTarget,
        selection: &Selection,
        account: &Account,
    ) -> Result<Vec<SelectedItem>> {
        let selection = match target.own_place().and_then(Place::space) {
            Some(space) => self.classify_number(space, selection, account).await?,
            None => selection.clone(),
        };
        (self
            .links
            .target_selection(self, target, &selection, true, account))
        .await
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
        let kind = self
            .links
            .space_number_kind(self, space, *number, account)
            .await?;
        Ok(Selection {
            kind: Some(kind),
            input: selection.input.clone(),
        })
    }

    /// Each issue and PR among `items` as the forge reports it now, in
    /// selection order; one that cannot be read records why.
    async fn read_views(
        &self,
        own: &ForgeRepo,
        account: &Account,
        items: &[SelectedItem],
        comments: bool,
    ) -> Vec<ItemView> {
        futures_util::future::join_all(items.iter().filter_map(|item| {
            let kind = item.kind().item()?;
            Some(async move {
                match self.viewed_item_forge(own, &item.url).await {
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

    /// The repository `item` is in: `own`, the one the view reads in, or
    /// the other one its URL names.
    async fn viewed_item_forge(&self, own: &ForgeRepo, item: &ItemRef) -> Result<ForgeRepo> {
        if item.repository() == own {
            return Ok(own.clone());
        }
        self.item_forge(item).await
    }
}
