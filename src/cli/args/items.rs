//! Arguments of commands for the issues and PRs linked to a workspace.
use clap::{Args, Subcommand, ValueEnum};

use crate::{cli::args::item_reference, model::Relation};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ItemsCommand {
    /// Link an issue or PR with a relation: `link 42 assigned`, `link pr 43 related`.
    ///
    /// Assigned items drive completion and cleanup; opened and related items
    /// are recorded only.
    Link {
        #[command(flatten)]
        items: RelationArgs,
    },
    /// Unlink a relation's items, one kind, or a selected issue or PR:
    /// `unlink assigned`, `unlink pr 43 related`.
    Unlink {
        #[command(flatten)]
        items: RelationArgs,
    },
    /// Wait for activity on linked items, one kind, or an explicit item.
    Watch {
        #[command(flatten)]
        items: ItemArgs,
        /// Return no updates when the wait expires (1–3600 seconds); omitted means unbounded.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=3600))]
        timeout: Option<u64>,
        /// Continue a wait across a daemon restart, as the installed release
        /// does after an upgrade restarted the daemon.
        #[arg(long, hide = true, requires = "workspace")]
        resume: bool,
    },
    /// Show linked items, one kind, or a selected issue, PR or CI run: `view ci 2765`.
    ///
    /// Issues and PRs show their status and discussion, and CI runs their jobs.
    View {
        #[command(flatten)]
        items: ItemArgs,
        /// Leave out comments and reviews.
        #[arg(long)]
        no_comments: bool,
        /// Show only this failed check's log, where the forge reads job logs.
        #[arg(long, value_name = "NAME", conflicts_with_all = ["job", "job_id"])]
        check: Option<String>,
        /// Show only the log of this job of a CI run.
        #[arg(long, value_name = "NAME", conflicts_with = "job_id")]
        job: Option<String>,
        /// Show only the log of the CI run's job with this ID, for jobs that share a name.
        #[arg(long, value_name = "ID")]
        job_id: Option<u64>,
        /// Open the items in the browser instead.
        #[arg(long, conflicts_with_all = ["no_comments", "check", "job", "job_id"])]
        web: bool,
    },
    /// Mark linked items, or the workspace when none are linked, ready for review at HEAD.
    Ready {
        #[command(flatten)]
        items: ItemArgs,
        /// Withdraw ready-for-review marks from the selected items.
        #[arg(long)]
        undo: bool,
        /// Return the PRs whose marks are withdrawn to draft, overriding [unready] draft.
        #[arg(long, requires = "undo", overrides_with = "no_draft")]
        draft: bool,
        /// Leave the PRs' draft state unchanged, overriding [unready] draft.
        #[arg(long, requires = "undo", overrides_with = "draft")]
        no_draft: bool,
    },
}

// An optional item selection followed by its relation.
#[derive(Debug, Args)]
pub struct RelationArgs {
    /// pr/issue, a number or URL, then assigned, opened or related.
    #[arg(required = true, num_args = 1..=3, value_name = "[ITEM] RELATION")]
    pub args: Vec<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

impl RelationArgs {
    /// Split the trailing relation from the item selection before it.
    pub fn split(self) -> anyhow::Result<(ItemArgs, Relation)> {
        let mut args = self.args;
        let last = args.pop().unwrap_or_default();
        let relation = Relation::from_str(&last, false).map_err(|_| {
            anyhow::anyhow!("end with a relation: assigned, opened or related (got {last})")
        })?;
        let mut items = args
            .into_iter()
            .map(|arg| item_reference(&arg).unwrap_or_else(|never| match never {}));
        let items = ItemArgs {
            kind_or_url: items.next(),
            item: items.next(),
            workspace: self.workspace,
        };
        Ok((items, relation))
    }
}

#[derive(Debug, Args)]
pub struct ItemArgs {
    /// pr/issue to select a kind, or an issue or PR number or URL.
    #[arg(value_parser = item_reference)]
    pub kind_or_url: Option<String>,
    /// Number or URL when a kind is supplied.
    #[arg(requires = "kind_or_url", value_parser = item_reference)]
    pub item: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

impl ItemArgs {
    /// Whether the selection names CI runs, which no number alone does.
    pub fn selects_ci(&self) -> bool {
        self.kind_or_url.as_deref() == Some("ci")
    }
}
