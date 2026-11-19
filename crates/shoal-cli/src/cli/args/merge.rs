//! Arguments of `shoal merge`, which merges a space's work, through its PR
//! on the forge or locally into the default branch, or a PR it names.
use anyhow::{Result, bail, ensure};
use clap::Args;

use crate::{
    cli::args::{
        ConfirmationArgs,
        changes::TargetRepo,
        items::{ItemArgs, ItemWord, item_word},
    },
    forge::items::{action::MergeMethod, link::LinkKind, reference::ItemInput},
};
use shoal_protocol::space_names::SpaceSelector;

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// pr, then a PR's number or URL, or the number or URL alone; omit to
    /// merge the space's work.
    #[arg(value_parser = item_word, value_name = "pr|NUMBER|URL")]
    pub kind_or_url: Option<ItemWord>,
    /// The PR's number or URL after pr.
    #[arg(requires = "kind_or_url", value_name = "NUMBER|URL")]
    pub item: Option<String>,
    /// Space, a workspace or chat, whose work merges, or whose repository a
    /// PR number is in; defaults to the current workspace or chat, or a
    /// picker.
    #[arg(short = 's', long, conflicts_with = "repo")]
    pub space: Option<SpaceSelector>,
    #[command(flatten)]
    pub repo: TargetRepo,
    /// How a PR merges, by [merge] method or else the repository's default
    /// on the forge when left out; a local merge takes only merge.
    #[arg(short = 'X', long, value_enum)]
    pub method: Option<MergeMethod>,
    /// The commit message of a squash merge.
    #[arg(long, conflicts_with = "cancel_auto")]
    pub message: Option<String>,
    /// Delete the PR's branch on the forge after merging.
    #[arg(long)]
    pub delete_branch: bool,
    /// Have the forge merge the PR once its required checks and approvals
    /// pass, or now when they already do.
    #[arg(long)]
    pub auto: bool,
    /// Withdraw the PR's automatic merge.
    #[arg(long, conflicts_with_all = ["method", "message", "delete_branch", "auto"])]
    pub cancel_auto: bool,
    /// Push the default branch to its upstream after a local merge,
    /// overriding [merge] push.
    #[arg(long, overrides_with = "no_push")]
    pub push: bool,
    /// Keep a local merge local, overriding [merge] push.
    #[arg(long, overrides_with = "push")]
    pub no_push: bool,
    #[command(flatten)]
    pub confirmation: ConfirmationArgs,
}

/// What a merge merges.
#[derive(Debug, PartialEq, Eq)]
pub enum Merged {
    /// The space's work, through its PR or locally, as the daemon decides.
    Work,
    /// The PR a number or URL names, else the space's linked one.
    Pr(Option<ItemInput>),
}

impl MergeArgs {
    /// What the arguments merge: a PR its item names, or the space's work
    /// without one.
    pub fn merged(&self) -> Result<Merged> {
        self.ensure_one_path()?;
        let Some(word) = &self.kind_or_url else {
            ensure!(
                self.repo.name.is_none(),
                "--repo names the repository of a PR; pass its number"
            );
            return Ok(Merged::Work);
        };
        ensure!(*word != ItemWord::Review, "{ONLY_PRS}");
        let items = ItemArgs {
            kind_or_url: Some(word.clone()),
            item: self.item.clone(),
            space: None,
        };
        let selection = items.selection()?;
        ensure!(
            selection.kind.is_none_or(|kind| kind == LinkKind::Pr),
            "{ONLY_PRS}"
        );
        Ok(Merged::Pr(selection.input))
    }

    /// Whether `--push` or `--no-push` overrides `[merge] push`, and how.
    pub fn push(&self) -> Option<bool> {
        (self.push || self.no_push).then_some(self.push)
    }

    /// Refuse flags of both paths together, which no merge takes, before
    /// anything asks which space merges.
    fn ensure_one_path(&self) -> Result<()> {
        let Some(local) = self.local_flag() else {
            return Ok(());
        };
        if let Some(pull) = self.pull_flag() {
            bail!(
                "{pull} applies to a PR merge and {local} to a local merge; pass the flags of one"
            );
        }
        if let Some(method @ (MergeMethod::Squash | MergeMethod::Rebase)) = self.method {
            bail!(
                "-X {method} merges only a PR and {local} applies to a local merge; pass the flags of one"
            );
        }
        Ok(())
    }

    /// Refuse the flags of a local merge for a merge through `pr`.
    pub fn ensure_pull_flags(&self, pr: &str) -> Result<()> {
        if let Some(flag) = self.local_flag() {
            bail!("{flag} applies to a local merge; this merge goes through {pr}");
        }
        Ok(())
    }

    /// Refuse the flags of a PR merge, and the methods a local merge does
    /// not take, for a local merge of the space `space`.
    pub fn ensure_local_flags(&self, space: &str) -> Result<()> {
        if let Some(flag) = self.pull_flag() {
            bail!("{flag} applies to a PR merge; {space} has no open PR, so it merges locally");
        }
        if let Some(method @ (MergeMethod::Squash | MergeMethod::Rebase)) = self.method {
            bail!(
                "a local merge fast-forwards or creates a merge commit, so it takes no -X {method}; \
                 {space} has no open PR, so it merges locally"
            );
        }
        Ok(())
    }

    /// The first flag given that only a PR merge takes.
    fn pull_flag(&self) -> Option<&'static str> {
        [
            (self.delete_branch, "--delete-branch"),
            (self.auto, "--auto"),
            (self.cancel_auto, "--cancel-auto"),
            (self.message.is_some(), "--message"),
        ]
        .into_iter()
        .find_map(|(given, flag)| given.then_some(flag))
    }

    /// The flag given that only a local merge takes.
    fn local_flag(&self) -> Option<&'static str> {
        match (self.push, self.no_push) {
            (true, _) => Some("--push"),
            (_, true) => Some("--no-push"),
            _ => None,
        }
    }
}

const ONLY_PRS: &str = "only PRs merge; pass a PR's number or URL";

#[cfg(test)]
mod tests;
