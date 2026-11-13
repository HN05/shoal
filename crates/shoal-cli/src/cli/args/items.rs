//! Arguments of commands for the issues, PRs and CI runs linked to a space,
//! a workspace or chat.
use clap::{Args, Subcommand, ValueEnum};

use anyhow::{Result, bail, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    forge::items::{
        link::{LinkKind, Selection},
        reference::ItemInput,
    },
    model::Relation,
    reviews::ReviewId,
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ItemsCommand {
    /// Link an issue, PR or CI run with a relation: `link 42 assigned`, `link ci 2765 related`.
    ///
    /// Assigned items drive completion and cleanup; opened and related items
    /// are recorded only. A CI run is opened or related, and watches follow
    /// it until it finishes.
    Link {
        #[command(flatten)]
        items: RelationArgs,
    },
    /// Unlink a relation's items, one kind, or a selected item:
    /// `unlink assigned`, `unlink pr 43 related`.
    Unlink {
        #[command(flatten)]
        items: RelationArgs,
    },
    /// Wait for activity on linked items, one kind, or an explicit item; `watch review` waits for a local review to end.
    #[command(group(clap::ArgGroup::new("watching_space").args(["space", "handed_over"])))]
    Watch {
        #[command(flatten)]
        items: ItemArgs,
        /// The registered repository of the item number a chat without a
        /// repository watches, by name, ID, path or remote URL.
        #[arg(short = 'r', long, value_name = "REPOSITORY")]
        repo: Option<String>,
        /// Return no updates when the wait expires (1–3600 seconds); omitted means unbounded.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: Option<u64>,
        /// The workspace the wait of an earlier release names when an upgrade
        /// hands it over to this one: an item watch with `--resume`, a
        /// review watch with its arguments unchanged.
        #[arg(long = "workspace", hide = true, conflicts_with = "space")]
        handed_over: Option<SpaceSelector>,
        /// Continue a wait across a daemon restart, as the installed release
        /// does after an upgrade restarted the daemon.
        #[arg(long, hide = true, requires = "watching_space")]
        resume: bool,
    },
    /// Show linked items, one kind, or a selected issue, PR, CI run or review: `view ci 2765`.
    ///
    /// Issues and PRs show their status and discussion, CI runs their jobs,
    /// and `view review` the workspace's latest local review with its findings.
    View(ViewArgs),
    /// Mark linked items, or the space when none are linked, ready for review.
    Ready {
        #[command(flatten)]
        items: ItemArgs,
        /// Withdraw ready-for-review marks from the selected items.
        #[arg(long)]
        undo: bool,
        /// Return the PRs whose marks are withdrawn to draft, overriding [pr] draft_on_unready.
        #[arg(long, requires = "undo", overrides_with = "no_draft")]
        draft: bool,
        /// Leave the PRs' draft state unchanged, overriding [pr] draft_on_unready.
        #[arg(long, requires = "undo", overrides_with = "draft")]
        no_draft: bool,
    },
}

// What `shoal view` shows and how.
#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("viewed_in").args(["space", "repo"])))]
pub struct ViewArgs {
    #[command(flatten)]
    pub items: ItemArgs,
    #[command(flatten)]
    pub repo: crate::cli::args::changes::RegisteredRepo,
    /// Leave out comments and reviews.
    #[arg(long)]
    pub no_comments: bool,
    /// Show only this failed check's log, where the forge reads job logs.
    #[arg(long, value_name = "NAME", conflicts_with = "job")]
    pub check: Option<String>,
    /// Show only the whole log of the CI run's job with this name or ID, in any state.
    #[arg(long, value_name = "NAME|ID")]
    pub job: Option<String>,
    /// Open the items in the browser instead.
    #[arg(long, conflicts_with_all = ["no_comments", "check", "job"])]
    pub web: bool,
}

// An optional item selection followed by its relation.
#[derive(Debug, Args)]
pub struct RelationArgs {
    /// pr/issue/ci, a number or URL, then assigned, opened or related.
    #[arg(required = true, num_args = 1..=3, value_name = "[ITEM] RELATION")]
    pub args: Vec<String>,
    /// Space, a workspace or chat, to use; defaults to the current workspace or
    /// chat, or a picker.
    #[arg(short = 's', long)]
    pub space: Option<SpaceSelector>,
}

impl RelationArgs {
    /// Split the trailing relation from the item selection before it.
    pub fn split(self) -> Result<(ItemArgs, Relation)> {
        let mut args = self.args;
        let last = args.pop().unwrap_or_default();
        let relation = Relation::from_str(&last, false).map_err(|_| {
            anyhow::anyhow!("end with a relation: assigned, opened or related (got {last})")
        })?;
        let mut items = args.into_iter();
        let items = ItemArgs {
            kind_or_url: items.next().as_deref().map(str::parse).transpose()?,
            item: items.next(),
            space: self.space,
        };
        Ok((items, relation))
    }
}

/// What an item selection's first argument names.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemWord {
    /// The linked items of one kind, or the one the next argument names.
    Kind(LinkKind),
    /// The space's latest local review, or the one the next argument numbers.
    Review,
    Item(ItemInput),
}

impl std::str::FromStr for ItemWord {
    type Err = anyhow::Error;

    fn from_str(word: &str) -> Result<Self> {
        if word == "review" {
            return Ok(Self::Review);
        }
        Ok(
            match LinkKind::ALL.iter().find(|kind| kind.as_str() == word) {
                Some(kind) => Self::Kind(*kind),
                None => Self::Item(word.parse()?),
            },
        )
    }
}

#[derive(Debug, Args)]
pub struct ItemArgs {
    /// pr/issue/ci/review to select a kind, an issue or PR number, or an
    /// issue, PR or CI run URL.
    #[arg(value_parser = item_word)]
    pub kind_or_url: Option<ItemWord>,
    /// Number or URL when a kind is supplied, or a review's ID.
    #[arg(requires = "kind_or_url")]
    pub item: Option<String>,
    /// Space, a workspace or chat, to use; defaults to the current workspace or
    /// chat, or a picker.
    #[arg(short = 's', long)]
    pub space: Option<SpaceSelector>,
}

/// clap parser for [`ItemWord`].
fn item_word(word: &str) -> Result<ItemWord, String> {
    word.parse().map_err(|error| format!("{error:#}"))
}

impl ItemArgs {
    /// Whether the selection names CI runs, which no number alone does.
    pub fn selects_ci(&self) -> bool {
        matches!(self.kind_or_url, Some(ItemWord::Kind(LinkKind::Ci)))
    }

    /// Whether the selection names a local review, by its ID when it has
    /// one: `view review 17`.
    pub fn selects_review(&self) -> bool {
        matches!(self.kind_or_url, Some(ItemWord::Review))
    }

    /// The review ID a review selection names.
    pub fn review_id(&self) -> Result<Option<ReviewId>> {
        self.item.as_deref().map(str::parse).transpose()
    }

    /// The issues, PRs or CI runs the arguments select, where a URL gives
    /// its kind and a bare number leaves it open.
    pub fn selection(&self) -> Result<Selection> {
        let input = self.item.as_deref().map(str::parse).transpose()?;
        Ok(match (&self.kind_or_url, input) {
            (None, _) => Selection::default(),
            (Some(ItemWord::Kind(kind)), input) => {
                ensure!(
                    input
                        .as_ref()
                        .and_then(ItemInput::kind)
                        .is_none_or(|named| named == *kind),
                    "link does not match the selected item kind"
                );
                Selection {
                    kind: Some(*kind),
                    input,
                }
            }
            (Some(ItemWord::Item(item)), None) => Selection {
                kind: item.kind(),
                input: Some(item.clone()),
            },
            (Some(ItemWord::Item(_)), Some(_)) => {
                bail!("use a number or URL, or pr/issue/ci followed by one")
            }
            (Some(ItemWord::Review), _) => bail!("a review is not an issue, PR or CI run"),
        })
    }
}

#[cfg(test)]
mod tests;
