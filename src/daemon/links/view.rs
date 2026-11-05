//! Issues and PRs the daemon reads for `shoal view` and apps, as the forge
//! reports them now, with the logs of failed checks.
use anyhow::Result;

use super::{reads::ReadPlace, space::LinkSpace};
use crate::{
    client::views::Viewed,
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::Account,
        items::link::{ItemKind, Selection},
        review::view::ItemView,
    },
    model::SelectedItem,
    protocol::ItemContext,
};

impl Manager {
    /// The items `selection` names in `context`, each issue and PR among
    /// them read as a `scoped` caller's agent account or the user's own
    /// logins, in selection order.
    pub async fn view_items(
        &self,
        context: &ItemContext,
        selection: &Selection,
        comments: bool,
        scoped: bool,
    ) -> Result<Viewed> {
        let account = self.read_account(scoped);
        let place = self.read_place(context).await?;
        let items = self.view_selection(&place, selection, &account).await?;
        // Nothing to read, so no forge is needed, as for a local repository.
        if !items.iter().any(|item| item.kind.item().is_some()) {
            return Ok(Viewed {
                items,
                views: Vec::new(),
            });
        }
        let own = self.read_place_forge(&place).await?;
        let mut views = self.read_views(&own, &account, &items, comments).await;
        // Logs are read as the agent account, whose token is the reading
        // one's for a scoped caller and the user's without one of its own.
        let own_token = !scoped && self.config().machine.forges.has_agent_token(&own.host);
        let agent = own_token.then(|| self.agent_account());
        own.read_failed_logs(agent.as_ref().unwrap_or(&account), &mut views)
            .await;
        Ok(Viewed { items, views })
    }

    /// The items `selection` names: in a space, where it may name linked
    /// ones, or in the repository of the registered checkout. An issue, PR
    /// or CI run URL of another repository selects that item.
    async fn view_selection(
        &self,
        place: &ReadPlace,
        selection: &Selection,
        account: &Account,
    ) -> Result<Vec<SelectedItem>> {
        match place {
            ReadPlace::Space(space) => {
                let selection = self.classify_number(space, selection, account).await?;
                Ok(self.select_items(space.id(), &selection, true).await?.items)
            }
            ReadPlace::Checkout(checkout) => {
                let item = (self.checkout_item(checkout, selection, true, account)).await?;
                Ok(vec![item])
            }
        }
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
        let (None, Some(input)) = (selection.kind, &selection.input) else {
            return Ok(selection.clone());
        };
        let Ok(number) = input.parse::<u64>() else {
            return Ok(selection.clone());
        };
        let kind = self.space_number_kind(space, number, account).await?;
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
            let kind = item.kind.item()?;
            Some(async move {
                match self.item_forge(own, kind, &item.url).await {
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

    /// The repository the item at `url` is in: `own`, or the other one its
    /// URL names.
    async fn item_forge(&self, own: &ForgeRepo, kind: ItemKind, url: &str) -> Result<ForgeRepo> {
        if own.item(kind.into(), url).is_ok() {
            return Ok(own.clone());
        }
        self.item_url_forge(kind.into(), url).await
    }
}
