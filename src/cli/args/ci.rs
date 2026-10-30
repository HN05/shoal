//! Arguments of `shoal ci`: listing, starting, rerunning, cancelling and
//! approving CI runs.
use clap::{Args, Subcommand};

use crate::cli::args::item_reference;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum CiCommand {
    /// List the newest CI runs, on the workspace's branch by default.
    List(ListArgs),
    /// Start a workflow run on a branch.
    ///
    /// It is a new run, not a rerun of a PR's checks; on Forgejo a failed
    /// check stays failed.
    Run(RunArgs),
    /// Rerun a run, or the failed checks on a PR's head; GitLab retries only
    /// a run's failed jobs.
    Rerun {
        #[command(flatten)]
        target: Target,
        /// Rerun only the run's failed jobs.
        #[arg(long, requires = "run")]
        failed: bool,
    },
    /// Cancel a running run, or the running checks on a PR's head.
    Cancel {
        #[command(flatten)]
        target: Target,
    },
    /// Approve a run that waits for approval, or every waiting run on a PR's
    /// head, with your own login; workspace processes cannot.
    ///
    /// Forges hold the runs of PRs from forks and first-time contributors
    /// until a maintainer approves them.
    Approve {
        #[command(flatten)]
        subject: Subject,
    },
}

// A run, or a PR and the checks on its head, to act on.
#[derive(Debug, Args)]
pub struct Target {
    #[command(flatten)]
    pub subject: Subject,
    /// Act only on this check of the PR, as `shoal view` names it; repeat for
    /// more. Defaults to every failed or running check.
    #[arg(long = "check", value_name = "NAME", conflicts_with = "run")]
    pub checks: Vec<String>,
}

// A run, or else a PR, to act on.
#[derive(Debug, Args)]
pub struct Subject {
    /// The run's web URL, as `shoal ci run` or a check links it, or the number
    /// in it; omit it to act on a PR's runs.
    pub run: Option<String>,
    /// Act on the runs on this PR's head, by number or URL; defaults to the
    /// linked PR.
    #[arg(long, value_name = "NUMBER_OR_URL", conflicts_with = "run", value_parser = item_reference)]
    pub pr: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

// Which runs `shoal ci list` lists.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Branch whose runs to list; defaults to the workspace's branch.
    #[arg(long)]
    pub branch: Option<String>,
    /// List the runs on this PR's head commit, by number or URL.
    #[arg(long, value_name = "NUMBER_OR_URL", conflicts_with = "branch", value_parser = item_reference)]
    pub pr: Option<String>,
    /// List only this workflow file's runs; GitLab has none.
    #[arg(long)]
    pub workflow: Option<String>,
    /// The most runs to list (1–100).
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}

// Arguments of `shoal ci run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Workflow file or ID, such as ci.yml, or a Bitbucket custom pipeline;
    /// GitLab runs the branch's pipeline and takes none.
    pub workflow: Option<String>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Branch to run on; defaults to the workspace's branch, the only one
    /// workspace processes may run on.
    #[arg(long)]
    pub branch: Option<String>,
    /// Pass a workflow input; repeat for more.
    #[arg(long = "input", value_name = "NAME=VALUE", value_parser = workflow_input)]
    pub inputs: Vec<(String, String)>,
}

fn workflow_input(input: &str) -> Result<(String, String), String> {
    match input.split_once('=') {
        Some((name, value)) if !name.trim().is_empty() => Ok((name.into(), value.into())),
        _ => Err("expected NAME=VALUE".into()),
    }
}
