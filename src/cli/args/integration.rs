use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SkillName {
    Worker,
    Orchestrator,
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
