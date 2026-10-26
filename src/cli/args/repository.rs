//! Arguments of `shoal repo` and `shoal forge`.
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{AgentParser, ConfirmationArgs},
};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum RepositoryCommand {
    /// Register and manage repositories; omit the subcommand for a menu.
    Repo {
        #[command(subcommand)]
        command: Option<RepoCommand>,
    },
    /// Inspect and access the current repository's forge.
    Forge {
        #[command(subcommand)]
        command: ForgeCommand,
    },
}

// Arguments of `shoal repo labels` and `shoal forge capabilities`.
#[derive(Debug, Args)]
pub struct RepositoryArgs {
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

// Arguments of `shoal forge api`.
#[derive(Debug, Args)]
pub struct ApiArgs {
    /// Path under the forge's API root, where `{owner}` and `{repo}` name the
    /// repository, such as `repos/{owner}/{repo}/branches`.
    pub path: String,
    /// HTTP method; defaults to POST with --input and GET otherwise.
    #[arg(short = 'X', long, value_enum, ignore_case = true)]
    pub method: Option<crate::forge::raw::HttpMethod>,
    /// JSON request body from a file, or `-` for standard input.
    #[arg(long, value_name = "FILE")]
    pub input: Option<PathBuf>,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

// Arguments of `shoal forge ci run`.
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

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ForgeCommand {
    /// List what the repository's forge supports.
    Capabilities(RepositoryArgs),
    /// Send a request to the repository's forge API and print its JSON response.
    ///
    /// Workspace processes send it as the agent account, and only GET unless
    /// `[api] agent_writes` lets agents send others; anyone else sends any
    /// request with your own logins.
    Api(ApiArgs),
    /// Start CI workflow runs on a branch.
    Ci {
        #[command(subcommand)]
        command: ForgeCiCommand,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ForgeCiCommand {
    /// Start a workflow run on a branch.
    ///
    /// It is a new run, not a rerun of a PR's checks; on Forgejo a failed
    /// check stays failed.
    Run(RunArgs),
    /// Rerun a workflow run; GitLab retries only its failed jobs.
    Rerun {
        #[command(flatten)]
        target: RunTarget,
        /// Rerun only the run's failed jobs.
        #[arg(long)]
        failed: bool,
    },
    /// Cancel a running workflow run.
    Cancel {
        #[command(flatten)]
        target: RunTarget,
    },
    /// Show a workflow run's state, its jobs and the log tails of failed jobs.
    View {
        #[command(flatten)]
        target: RunTarget,
        /// Show only this job's log.
        #[arg(long, value_name = "NAME", conflicts_with = "job_id")]
        job: Option<String>,
        /// Show only the log of the job with this ID, for jobs that share a name.
        #[arg(long, value_name = "ID")]
        job_id: Option<u64>,
    },
    /// List the newest workflow runs, on the workspace's branch by default.
    List {
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Branch whose runs to list; defaults to the workspace's branch.
        #[arg(long)]
        branch: Option<String>,
        /// List only this workflow file's runs; GitLab has none.
        #[arg(long)]
        workflow: Option<String>,
        /// The most runs to list (1–100).
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}

// A workflow run to act on.
#[derive(Debug, Args)]
pub struct RunTarget {
    /// The run's web URL, as `shoal forge ci run` or a check links it, or the number in it.
    pub run: String,
    /// Workspace to use; defaults to the current workspace or picker.
    #[arg(long)]
    pub workspace: Option<String>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum RepoCommand {
    /// List the repository's labels with their descriptions.
    Labels(RepositoryArgs),
    /// Fetch a repository's remote and fast-forward its default branch.
    ///
    /// Never pushes or changes workspace branches; rebase or merge onto the
    /// updated branch with Git.
    Sync {
        /// Registered repository; defaults to the current checkout or workspace.
        repository: Option<String>,
    },
    /// Show or replace repository configuration stored locally by Shoal.
    Config {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        /// Import TOML as the complete config for all this repository's workspaces.
        #[arg(long, conflicts_with = "clear", value_hint = clap::ValueHint::FilePath)]
        file: Option<PathBuf>,
        /// Remove the local config and use each worktree's repository config again.
        #[arg(long)]
        clear: bool,
    },
    /// Register a local checkout (no remote required), or clone a repository URL.
    Add {
        source: String,
        #[arg(long)]
        name: Option<String>,
        /// Clone this URL into this exact directory instead of root_dir/<name>/.checkout.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Rename {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        /// New name; omit to enter one.
        name: Option<String>,
    },
    /// Have an agent write the repository's Shoal config in a shoal-setup workspace.
    Setup {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        /// Agent to start; defaults to default_agent, otherwise a picker.
        #[arg(long, value_parser = AgentParser)]
        agent: Option<Agent>,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Move every repository directory, with its workspaces and clone, into root_dir.
    Migrate {
        /// New root directory, saved as root_dir first; omit to use the configured one.
        #[arg(value_hint = clap::ValueHint::DirPath)]
        root: Option<PathBuf>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Delete a repository checkout and all its Shoal workspaces and resources.
    ///
    /// Uncommitted and unpushed work is permanently lost.
    #[command(alias = "remove")]
    Rm {
        /// Registered repository; omit to choose one.
        repository: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    Ls,
}
