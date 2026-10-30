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

// Arguments of `shoal repo labels`, `shoal forge capabilities` and
// `shoal forge reactions`.
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

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ForgeCommand {
    /// List what the repository's forge supports.
    Capabilities(RepositoryArgs),
    /// List the reactions the repository's forge allows.
    Reactions(RepositoryArgs),
    /// Send a request to the repository's forge API and print its JSON response.
    ///
    /// Workspace processes send it as the agent account, and only GET unless
    /// `[permissions] api_writes` lets agents send others; anyone else sends any
    /// request with your own logins.
    Api(ApiArgs),
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
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
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
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        /// New name; omit to enter one.
        name: Option<String>,
    },
    /// Have an agent write the repository's Shoal config in a shoal-setup workspace.
    Setup {
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        /// Agent to start; defaults to agent.default, otherwise a picker.
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
        /// Keep root_dir a regular directory instead of a link to one Spotlight skips (macOS).
        #[arg(long)]
        regular_dir: bool,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    /// Delete a repository checkout and all its Shoal workspaces and resources.
    ///
    /// Uncommitted and unpushed work is permanently lost.
    #[command(alias = "remove")]
    Rm {
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    Ls,
}
