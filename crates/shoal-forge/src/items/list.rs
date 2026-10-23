//! Issues and PRs a repository lists: those a search matches, or the open
//! ones for choosing one interactively.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{
        item::{Item, ItemState},
        link::ItemKind,
    },
    state::states,
};

states!(StateFilter: ValueEnum {
    Open => "open",
    /// Closed, and merged for PRs.
    Closed => "closed",
    All => "all",
});

/// The most items one listing returns.
pub const MAX_LIMIT: usize = 1000;

/// How many open items a picker offers.
const PICKER_LIMIT: usize = 200;

/// Which issues or PRs a listing returns, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Search {
    pub state: StateFilter,
    /// Labels every listed item has.
    pub labels: Vec<String>,
    /// Login of the listed items' author.
    pub author: Option<String>,
    /// Text the forge's search matches in titles and descriptions.
    pub text: Option<String>,
    pub limit: usize,
}

impl Search {
    fn open(limit: usize) -> Self {
        Self {
            state: StateFilter::Open,
            labels: Vec::new(),
            author: None,
            text: None,
            limit,
        }
    }

    /// Whether an item in `state` is one the search asks for.
    pub(crate) fn includes(&self, state: ItemState) -> bool {
        match self.state {
            StateFilter::Open => state == ItemState::Open,
            StateFilter::Closed => state != ItemState::Open,
            StateFilter::All => true,
        }
    }

    /// Refuse a limit out of range or an empty filter, before anything is
    /// chosen or sent.
    pub fn check(&self) -> Result<()> {
        ensure!(
            (1..=MAX_LIMIT).contains(&self.limit),
            "the limit must be between 1 and {MAX_LIMIT}"
        );
        let blank = |text: &str| text.trim().is_empty();
        ensure!(
            !self.labels.iter().any(|label| blank(label))
                && !self.author.as_deref().is_some_and(blank)
                && !self.text.as_deref().is_some_and(blank),
            "a label, author or search text cannot be empty"
        );
        Ok(())
    }

    /// The capabilities the forge needs for the search.
    fn needs(&self) -> Option<Capability> {
        (!self.labels.is_empty()).then_some(Capability::Labels)
    }
}

impl ForgeRepo {
    /// Up to `search.limit` issues or PRs that `search` matches, newest first.
    pub async fn list(
        &self,
        account: &Account,
        kind: ItemKind,
        search: &Search,
    ) -> Result<Vec<Item>> {
        search.check()?;
        self.require_all(search.needs())?;
        let mut items = self.kind.list_items(self, account, kind, search).await?;
        items.retain(|item| search.includes(item.state));
        items.truncate(search.limit);
        Ok(items)
    }

    pub async fn open_issues(&self, account: &Account) -> Result<Vec<Item>> {
        self.list(account, ItemKind::Issue, &Search::open(PICKER_LIMIT))
            .await
    }

    /// Open issues labelled `label`, newest first. Unlike a picker's, the
    /// listing reaches back far enough to hold the oldest of them.
    pub async fn open_issues_labelled(&self, account: &Account, label: &str) -> Result<Vec<Item>> {
        let search = Search {
            labels: vec![label.to_owned()],
            ..Search::open(MAX_LIMIT)
        };
        self.list(account, ItemKind::Issue, &search).await
    }

    pub async fn open_pulls(&self, account: &Account) -> Result<Vec<Item>> {
        self.list(account, ItemKind::Pr, &Search::open(PICKER_LIMIT))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_includes_merged_and_searches_refuse_bad_filters() {
        let mut search = Search::open(30);
        assert!(search.includes(ItemState::Open));
        assert!(!search.includes(ItemState::Merged));
        search.state = StateFilter::Closed;
        assert!(search.includes(ItemState::Merged));
        assert!(!search.includes(ItemState::Open));
        assert!(search.check().is_ok());
        assert_eq!(search.needs(), None);

        for change in [
            |search: &mut Search| search.limit = 0,
            |search: &mut Search| search.limit = MAX_LIMIT + 1,
            |search: &mut Search| search.text = Some(" ".into()),
            |search: &mut Search| search.author = Some(String::new()),
            |search: &mut Search| search.labels = vec![String::new()],
        ] {
            let mut search = Search::open(30);
            change(&mut search);
            assert!(search.check().is_err(), "{search:?}");
        }
        search.labels = vec!["bug".into()];
        assert!(search.check().is_ok());
        assert_eq!(search.needs(), Some(Capability::Labels));
    }
}
