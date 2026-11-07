//! Arguments of `shoal repo` and `shoal forge`.
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    agent::Agent,
    cli::args::{AgentParser, ConfirmationArgs, ModelArgs},
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
    /// Read another repository: a registered repository's name, OWNER/NAME
    /// on the current repository's forge, or a repository URL on any forge;
    /// defaults to the current workspace's or checkout's repository.
    #[arg(short = 'r', long, value_name = "REPOSITORY")]
    pub repo: Option<String>,
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
    /// Send it for another repository, which `{owner}` and `{repo}` then
    /// name: a registered repository's name, OWNER/NAME on the current
    /// repository's forge, or a repository URL on any forge; defaults to the
    /// current workspace's or checkout's repository.
    #[arg(short = 'r', long, value_name = "REPOSITORY")]
    pub repo: Option<String>,
}

// Arguments of `shoal forge download`.
#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// URL of an image or attached file on the repository's forge, such as
    /// one in a PR's description.
    pub url: String,
    /// File to write, or `-` for standard output.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// Download from another repository's forge: a registered repository's
    /// name, OWNER/NAME on the current repository's forge, or a repository
    /// URL on any forge; defaults to the current workspace's or checkout's
    /// repository.
    #[arg(short = 'r', long, value_name = "REPOSITORY")]
    pub repo: Option<String>,
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
    /// `[permissions] api_writes` lets agents send others to the workspace's
    /// repository; anyone else sends any request with your own logins.
    Api(ApiArgs),
    /// Download an image or attached file from the repository's forge.
    ///
    /// Uses the login Shoal uses there, so files of a private repository
    /// load; URLs on other hosts are refused.
    Download(DownloadArgs),
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
    /// Show the path and contents of the repository's local config.
    Config {
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
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
    /// Have an agent write the repository's Shoal config in a workspace on
    /// shoal-setup, then commit it or save it locally, as you choose.
    Setup {
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        /// Run in this terminal instead of a new tab or detached session.
        #[arg(short = 'H', long)]
        here: bool,
        /// Agent to start; defaults to agent.default, otherwise a picker.
        #[arg(short = 'a', long, value_parser = AgentParser)]
        agent: Option<Agent>,
        #[command(flatten)]
        model: ModelArgs,
        /// Arguments forwarded to the agent.
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Delete a repository checkout and all its Shoal workspaces and resources.
    ///
    /// Uncommitted and unpushed work is permanently lost.
    Rm {
        /// Registered repository; defaults to the current checkout or workspace, else a picker.
        repository: Option<String>,
        #[command(flatten)]
        confirmation: ConfirmationArgs,
    },
    Ls,
}
