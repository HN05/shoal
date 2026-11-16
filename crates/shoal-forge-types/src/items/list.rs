//! Searches of a repository's issues and PRs: those a search matches, or
//! the open ones for choosing one interactively.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_states::states;

use crate::{capability::Capability, items::item::ItemState};

states!(StateFilter: ValueEnum {
    Open => "open",
    /// Closed, and merged for PRs.
    Closed => "closed",
    All => "all",
});

states!(Matching: ValueEnum {
    /// The words themselves.
    Keyword => "keyword",
    /// What the words mean, as the forge's search by meaning finds it.
    Meaning => "meaning",
    /// Either: every keyword match, with the meaning matches it misses.
    Both => "both",
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
    /// How the forge matches `text`.
    pub matching: Matching,
    pub limit: usize,
}

impl Search {
    /// The open items a picker offers to choose from.
    pub fn picker() -> Self {
        Self::open(PICKER_LIMIT)
    }

    /// Open items labelled `label`. Unlike a picker's, the listing reaches
    /// back far enough to hold the oldest of them.
    pub fn labelled(label: &str) -> Self {
        Self {
            labels: vec![label.to_owned()],
            ..Self::open(MAX_LIMIT)
        }
    }

    fn open(limit: usize) -> Self {
        Self {
            state: StateFilter::Open,
            labels: Vec::new(),
            author: None,
            text: None,
            matching: Matching::Keyword,
            limit,
        }
    }

    /// Whether an item in `state` is one the search asks for.
    pub fn includes(&self, state: ItemState) -> bool {
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

    /// How the forge matches the search's text: by keyword for a search
    /// without any, which matches every item.
    pub fn matched_by(&self) -> Matching {
        match self.text {
            Some(_) => self.matching,
            None => Matching::Keyword,
        }
    }

    /// The capabilities the forge needs for the search. Matching `both`
    /// ways needs none, since a forge without a search by meaning matches
    /// by keyword alone.
    pub fn needs(&self) -> Vec<Capability> {
        let labels = (!self.labels.is_empty()).then_some(Capability::Labels);
        let meaning = (self.matched_by() == Matching::Meaning).then_some(Capability::MeaningSearch);
        labels.into_iter().chain(meaning).collect()
    }
}

#[cfg(test)]
mod tests;
