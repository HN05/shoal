//! Clap definitions of the commands' arguments, one module per command
//! group, and the argument types several groups share.
pub mod approvals;
pub mod changes;
pub mod ci;
pub(in crate::cli) mod command_line;
pub mod coordination;
pub mod create;
pub mod diagnostics;
pub mod integration;
pub mod internal;
pub mod items;
pub mod lifecycle;
pub mod repository;
pub mod resources;
pub mod service;
pub mod sessions;
pub mod target;
pub mod transfer;
pub mod workspace;

use std::path::PathBuf;

use clap::Args;

use crate::agent::Agent;
use crate::client::space_names::SpaceSelector;

#[derive(Debug, Default, Args)]
pub struct ConfirmationArgs {
    /// Confirm without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

// The workspace a listing covers, from its command's `--workspace` and
// `--all`.
#[derive(Debug, Default)]
pub struct WorkspaceScope {
    pub workspace: Option<SpaceSelector>,
    /// All workspaces.
    pub all: bool,
}

// The spaces of a command that acts on several: workspaces, and chats where
// it acts on them.
#[derive(Debug, Default, Args)]
pub struct SpaceTargets {
    #[arg(value_name = "SPACE")]
    pub space: Vec<SpaceSelector>,
    /// All workspaces.
    #[arg(short = 'A', long, conflicts_with = "space")]
    pub all: bool,
}

// A description or comment, given inline or read from a file.
#[derive(Debug, Default, Args)]
pub struct BodyArgs {
    /// Text of the description or comment.
    #[arg(long)]
    pub body: Option<String>,
    /// Read the text from a file.
    #[arg(long, value_name = "PATH", conflicts_with = "body")]
    pub body_file: Option<PathBuf>,
}

// `--model` and `--effort` of a command that starts an agent.
#[derive(Debug, Default, Clone, clap::Args)]
pub struct ModelArgs {
    /// Model the agent runs with, overriding [agents.<name>] model and [agent] model.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub model: Option<String>,
    /// Reasoning effort the agent runs with, overriding [agents.<name>] effort and [agent] effort.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub effort: Option<String>,
}

impl From<ModelArgs> for crate::agent::ModelChoice {
    fn from(args: ModelArgs) -> Self {
        Self {
            model: args.model,
            effort: args.effort,
        }
    }
}

/// An issue, PR or CI run argument: its number, `#42` included, or URL.
pub(in crate::cli) fn item_input(
    input: &str,
) -> Result<crate::forge::items::reference::ItemInput, String> {
    input.parse().map_err(|error| format!("{error:#}"))
}

/// An issue argument: its number or URL.
pub(in crate::cli) fn issue_input(
    input: &str,
) -> Result<crate::forge::items::reference::ItemInput, String> {
    kind_input(input, crate::forge::items::link::LinkKind::Issue)
}

/// A PR argument: its number or URL.
pub(in crate::cli) fn pull_input(
    input: &str,
) -> Result<crate::forge::items::reference::ItemInput, String> {
    kind_input(input, crate::forge::items::link::LinkKind::Pr)
}

/// A CI run argument: the number in its web URL, or the URL.
pub(in crate::cli) fn run_input(
    input: &str,
) -> Result<crate::forge::items::reference::ItemInput, String> {
    kind_input(input, crate::forge::items::link::LinkKind::Ci)
}

/// A `kind` item argument: its number or a URL of that kind.
fn kind_input(
    input: &str,
    kind: crate::forge::items::link::LinkKind,
) -> Result<crate::forge::items::reference::ItemInput, String> {
    use crate::forge::{IssueInput, items::reference::ItemRef};
    match IssueInput::parse(input) {
        IssueInput::Url => ItemRef::parse_kind(kind, input)
            .map(Into::into)
            .map_err(|error| format!("{error:#}")),
        IssueInput::Number => item_input(input),
        IssueInput::Invalid if input.starts_with('#') => item_input(input),
        IssueInput::Invalid => Err(format!(
            "expected the {} number or URL, not {input}",
            kind.noun()
        )),
    }
}

/// The change one `--permission KEY=VALUE` makes, `KEY=mode` dropping the
/// key's given value, refusing an unknown key or value before anything is
/// asked.
pub(in crate::cli) fn permission_assignment(
    text: &str,
) -> Result<crate::config::permissions::PermissionChange, String> {
    crate::config::permissions::PermissionChange::assigned(text)
        .map_err(|error| format!("{error:#}"))
}

/// clap parser for [`Agent`] that also advertises its values for completion.
#[derive(Clone)]
pub struct AgentParser;

impl clap::builder::TypedValueParser for AgentParser {
    type Value = Agent;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Agent, clap::Error> {
        let value = value
            .to_str()
            .ok_or_else(|| clap::Error::new(clap::error::ErrorKind::InvalidUtf8).with_cmd(cmd))?;
        value.parse().map_err(|_| {
            let mut error = clap::Error::new(clap::error::ErrorKind::InvalidValue).with_cmd(cmd);
            if let Some(arg) = arg {
                error.insert(
                    clap::error::ContextKind::InvalidArg,
                    clap::error::ContextValue::String(arg.to_string()),
                );
            }
            error.insert(
                clap::error::ContextKind::InvalidValue,
                clap::error::ContextValue::String(value.to_owned()),
            );
            error.insert(
                clap::error::ContextKind::ValidValue,
                clap::error::ContextValue::Strings(crate::cli::agents::builtin_names()),
            );
            error
        })
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            crate::cli::agents::builtin_names()
                .into_iter()
                .map(clap::builder::PossibleValue::new),
        ))
    }
}
