//! `shoal run` and `shoal <name>`: configured commands use the same workspace
//! selection and wrapper as arbitrary commands.
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
};

use anyhow::{Context as _, Result};
use clap::{CommandFactory, Parser};
use serde::Serialize;

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        recent,
        ui::{self, Fallback},
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    config::{
        Config,
        named_commands::{self, Commands},
        repo::{ConfigLayer, ConfigLayers},
        resolve::Stack,
    },
    execution,
    model::{DiffBase, Workspace},
    paths::Paths,
    protocol::{ConfigTarget, Method},
};

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CommandDefinition {
    name: String,
    argv: Vec<String>,
    layer: ConfigLayer,
    bare_name: BareName,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum BareName {
    Shorthand,
    BuiltIn,
}

pub(super) async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let global = Config::load(&ctx.paths)?;
    let layers = listing_layers(ctx, workspace).await?;
    let definitions = Stack::new(&global, &layers).named(|config| &config.commands);

    let built_ins: BTreeSet<_> = crate::cli::Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect();
    let definitions: Vec<_> = definitions
        .into_iter()
        .map(|(name, (argv, layer))| {
            let bare_name = if built_ins.contains(&name) {
                BareName::BuiltIn
            } else {
                BareName::Shorthand
            };
            CommandDefinition {
                name,
                argv,
                layer,
                bare_name,
            }
        })
        .collect();
    ctx.show(&definitions, |definitions| {
        for command in definitions {
            outln!(
                "{} = {} ({}{})",
                command.name,
                serde_json::to_string(&command.argv).expect("serialize command argv"),
                command.layer,
                if command.bare_name == BareName::BuiltIn {
                    "; bare name is built-in"
                } else {
                    ""
                }
            );
        }
    })?;
    Ok(0)
}

/// An explicit workspace takes precedence over the current directory.
async fn listing_layers(ctx: &Context, workspace: Option<String>) -> Result<ConfigLayers> {
    if workspace.is_some() {
        let workspace =
            ui::select_workspace(ctx, workspace, Fallback::CurrentDirectoryOnly).await?;
        return workspace_layers(ctx, workspace).await;
    }
    current_layers(ctx).await
}

/// The repository layers of the current workspace; none without a running
/// daemon or a current workspace.
async fn current_layers(ctx: &Context) -> Result<ConfigLayers> {
    if client::status(&ctx.paths).await?.is_none() {
        return Ok(ConfigLayers::default());
    }
    let workspaces = client::workspaces(&ctx.paths).await?;
    let current = std::env::current_dir().ok();
    let context = WorkspaceContext::from_directory(&workspaces, current.as_deref());
    let workspace = context.resolve(None, crate::env::is_scoped(), ScopeOrder::AfterDirectory);
    let Some(workspace) = workspace else {
        return Ok(ConfigLayers::default());
    };
    workspace_layers(ctx, workspace.id.clone()).await
}

async fn workspace_layers(ctx: &Context, workspace: String) -> Result<ConfigLayers> {
    Ok(*request::<Box<ConfigLayers>>(
        &ctx.paths,
        Method::LayeredConfig {
            target: ConfigTarget::Workspace(workspace),
        },
    )
    .await?)
}

#[derive(Parser)]
struct Invocation {
    workspace: Option<String>,
    #[arg(last = true)]
    args: Vec<OsString>,
}

/// The name, workspace and arguments of `shoal <name>`; `None` once help
/// has been printed.
pub(super) type Invoked = (String, Option<String>, Vec<OsString>);

pub(super) fn invocation(words: Vec<OsString>) -> Result<Option<Invoked>> {
    let name = words[0]
        .to_str()
        .context("command name must be UTF-8")?
        .to_owned();
    match Invocation::try_parse_from(words) {
        Ok(invocation) => Ok(Some((name, invocation.workspace, invocation.args))),
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            crate::cli::output::print_clap_output(&error);
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) async fn run(
    ctx: &Context,
    name: &str,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let known_globally = Config::load(&ctx.paths)?
        .resolve(&ConfigLayers::default())?
        .commands
        .contains_key(name);
    let fallback = if known_globally {
        Fallback::CurrentDirectory
    } else {
        Fallback::CurrentDirectoryOnly
    };
    let workspace = match ui::select_workspace(ctx, workspace, fallback).await {
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => {
            let defining = defining_workspaces(ctx, name).await?;
            if defining.is_empty() {
                return Err(error
                    .context("repository-only commands require a current or explicit workspace")
                    .context(unknown_command(name)));
            }
            let workspace = ui::pick_workspace(ctx, defining).await?;
            recent::record(ctx, &workspace).await;
            workspace
        }
        workspace => workspace?,
    };
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    let inspection = client::inspect(&ctx.paths, workspace.clone()).await?;
    let command = expand(
        &ctx.paths,
        &settings.commands,
        name,
        &inspection.workspace,
        args,
    )
    .await?;
    execution::run_command(&ctx.paths, workspace, command).await
}

/// Workspaces whose repository configuration defines `name`; none for
/// noninteractive callers, who must name the workspace.
async fn defining_workspaces(ctx: &Context, name: &str) -> Result<Vec<Workspace>> {
    let mut defining = Vec::new();
    if !ctx.interactive() {
        return Ok(defining);
    }
    for workspace in client::workspaces(&ctx.paths).await? {
        let target = ConfigTarget::Workspace(workspace.id.clone());
        // A workspace whose configuration cannot load does not define it.
        if client::settings(&ctx.paths, target)
            .await
            .is_ok_and(|settings| settings.commands.contains_key(name))
        {
            defining.push(workspace);
        }
    }
    Ok(defining)
}

async fn expand(
    paths: &Paths,
    commands: &Commands,
    name: &str,
    workspace: &Workspace,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    expand_with_fields(
        paths,
        commands,
        name,
        workspace,
        args,
        &[("{prompt}", OsStr::new(""))],
    )
    .await
}

/// `name`'s command for `workspace`, with `extra_fields` beside the
/// workspace's own placeholders.
pub(crate) async fn expand_with_fields(
    paths: &Paths,
    commands: &Commands,
    name: &str,
    workspace: &Workspace,
    args: Vec<OsString>,
    extra_fields: &[(&str, &OsStr)],
) -> Result<Vec<OsString>> {
    let argv = commands.get(name).ok_or_else(|| unknown_command(name))?;
    let base = diff_base(paths, workspace, argv).await?;
    let mut fields = named_commands::workspace_fields(workspace, base.as_deref());
    fields.extend_from_slice(extra_fields);
    Ok(named_commands::substitute(argv, &fields, args))
}

/// The workspace's diff base commit when `argv` uses it.
async fn diff_base(
    paths: &Paths,
    workspace: &Workspace,
    argv: &[String],
) -> Result<Option<String>> {
    if !named_commands::uses_diff_base(argv) {
        return Ok(None);
    }
    let base = request::<DiffBase>(
        paths,
        Method::DiffBase {
            workspace: workspace.id.clone(),
        },
    )
    .await?;
    Ok(Some(base.commit))
}

/// `name`'s command with only `fields` known, for launches outside a workspace,
/// refused when it uses a placeholder only a workspace has a value for.
pub(super) fn expand_outside_workspace(
    commands: &Commands,
    name: &str,
    fields: &[(&str, &OsStr)],
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    let argv = commands.get(name).ok_or_else(|| unknown_command(name))?;
    if let Some(placeholder) = named_commands::workspace_only_placeholder(argv) {
        anyhow::bail!(
            "the {name} command uses {placeholder}, which has no value outside a workspace"
        );
    }
    Ok(named_commands::substitute(argv, fields, args))
}

fn unknown_command(name: &str) -> anyhow::Error {
    let mut message = format!("unknown command {name:?} in the current context");
    // Let clap suggest from the actual built-ins, without the custom-command fallback.
    if let Err(error) = crate::cli::Cli::command()
        .allow_external_subcommands(false)
        .external_subcommand_value_parser(None)
        .try_get_matches_from(["shoal", name])
        && let Some(clap::error::ContextValue::Strings(suggestions)) =
            error.get(clap::error::ContextKind::SuggestedSubcommand)
        && !suggestions.is_empty()
    {
        let suggestions = suggestions
            .iter()
            .map(|suggestion| format!("`shoal {suggestion}`"))
            .collect::<Vec<_>>()
            .join(" or ");
        message.push_str(&format!("; did you mean {suggestions}?"));
    }
    message.push_str(
        "; run `shoal --help` for built-in commands or `shoal run` for configured commands",
    );
    anyhow::anyhow!(message)
}
