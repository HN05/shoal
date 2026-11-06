//! Arguments of `shoal skill` and `shoal shell`.
use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum IntegrationCommand {
    /// Show or install Shoal instructions for agents.
    Skill {
        #[command(subcommand)]
        command: Option<SkillCommand>,
    },
    /// Print shell integration for Bash or Zsh.
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum SkillCommand {
    /// Install or refresh the bundled skills for configured AI tools (no daemon needed).
    Install {
        /// Install for one AI tool, or all configured tools by default.
        #[arg(default_value = "all")]
        agent: String,
        /// Replace skills changed since Shoal installed them without asking.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ShellCommand {
    /// Generate shell completion scripts.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    Init,
    #[command(hide = true)]
    Recover {
        path: PathBuf,
    },
}
