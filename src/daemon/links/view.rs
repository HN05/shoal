//! Issues and PRs the daemon reads for `shoal view` and apps, as the forge
//! reports them now, with the logs of failed checks.
use anyhow::{Context, Result};

use super::{actions::NOTHING_LINKED, reads::ReadPlace, space::LinkSpace};
use crate::{
    client::views::Viewed,
    daemon::workspace::Manager,
    forge::{
        ForgeRepo,
        account::Account,
        capability::Capability,
        items::link::{ItemKind, LinkKind, Selection},
        review::view::ItemView,
    },
    model::{Repository, Selected, SelectedItem},
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
        self.read_failed_logs(context, &own, &mut views).await;
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
                let item = self.checkout_item(checkout, selection, account).await?;
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
        let kind = match linked_kind(space, number).await? {
            Some(kind) => kind,
            None => (self.space_forge(space).await?)
                .item_kind(account, number)
                .await?
                .into(),
        };
        Ok(Selection {
            kind: Some(kind),
            input: selection.input.clone(),
        })
    }

    /// The item `selection` names in the repository of the registered
    /// checkout, where nothing is linked, or the item of another repository
    /// its URL names. A number without a kind takes the one the forge
    /// reports for it.
    async fn checkout_item(
        &self,
        checkout: &Repository,
        selection: &Selection,
        account: &Account,
    ) -> Result<SelectedItem> {
        let input = selection.input.as_deref().context(NOTHING_LINKED)?;
        if let Some(item) = self.item_elsewhere(&checkout.path, selection).await? {
            return Ok(item);
        }
        let forge = self.checkout_forge(checkout).await?;
        let kind = match selection.kind {
            Some(kind) => kind,
            None => forge.item_kind(account, input.parse()?).await?.into(),
        };
        let (_, url) = match kind {
            LinkKind::Issue => forge.issue(input)?,
            LinkKind::Pr => forge.pull(input)?,
            LinkKind::Ci => forge.run(input)?,
        };
        Ok(SelectedItem { url, kind })
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
        match kind {
            ItemKind::Issue if own.issue(url).is_err() => self.issue_url_forge(url).await,
            ItemKind::Pr if own.pull(url).is_err() => self.pull_forge(url).await,
            _ => Ok(own.clone()),
        }
    }

    /// Add the logs of the failed checks of the PRs among `views` in the
    /// repository of `context`, recording those it cannot read.
    async fn read_failed_logs(
        &self,
        context: &ItemContext,
        own: &ForgeRepo,
        views: &mut [ItemView],
    ) {
        if !own.supports(Capability::ReadJobLogs) {
            return;
        }
        for view in views {
            if own.pull(&view.url).is_err() {
                continue;
            }
            let failed: Vec<String> = view
                .details
                .iter()
                .filter_map(|details| details.pr.as_ref())
                .flat_map(|pr| &pr.checks)
                .filter(|check| check.failed())
                .map(|check| check.name.clone())
                .collect();
            let selection = Selection {
                kind: Some(LinkKind::Pr),
                input: Some(view.url.clone()),
            };
            for name in failed {
                match self.check_log(context, &selection, &name).await {
                    Ok(log) => view.check_logs.push(log),
                    Err(error) => view
                        .errors
                        .push(format!("{name}: log unavailable: {error:#}")),
                }
            }
        }
    }
}

/// The kind of the item numbered `number` linked to `space` under any
/// relation.
async fn linked_kind(space: &LinkSpace, number: u64) -> Result<Option<LinkKind>> {
    let linked = Selected {
        path: space.origin_path().to_owned(),
        items: (space.links().items().into_iter())
            .map(|item| SelectedItem {
                url: item.url,
                kind: item.kind,
            })
            .collect(),
    };
    // A number names an item of the space's repository; issues opened in
    // another one share its numbers.
    let remote = crate::repository::remote_url_from_path(space.origin_path())
        .await?
        .context("item lookup needs an origin remote")?;
    linked.kind_of(&remote, number)
}
