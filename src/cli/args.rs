//! Clap definitions of the commands' arguments, one module per command
//! group, and the argument types several groups share.
pub mod agents;
pub mod approvals;
pub mod changes;
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
pub mod workspace;

use std::path::PathBuf;

use clap::Args;

use crate::{agent::Agent, forge::IssueInput};

#[derive(Debug, Default, Args)]
pub struct ConfirmationArgs {
    /// Confirm without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Default, Args)]
pub struct WorkspaceScope {
    pub workspace: Option<String>,
    /// All workspaces.
    #[arg(long, conflicts_with = "workspace")]
    pub all: bool,
}

// The workspaces of a command that acts on several.
#[derive(Debug, Default, Args)]
pub struct WorkspaceTargets {
    #[arg(value_name = "WORKSPACE")]
    pub workspace: Vec<String>,
    /// All workspaces.
    #[arg(long, conflicts_with = "workspace")]
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

/// An issue or PR argument, where `#42` means `42`.
pub(in crate::cli) fn item_reference(input: &str) -> Result<String, std::convert::Infallible> {
    Ok(match input.strip_prefix('#') {
        Some(number) if IssueInput::parse(number) == IssueInput::Number => number.to_owned(),
        _ => input.to_owned(),
    })
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
