//! `shoal run <name>`: configured commands use the same workspace selection
//! and wrapper as arbitrary commands.
use crate::client::space_ids::SpaceId;
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use crate::client::space_names::SpaceSelector;

use crate::client::output::configured::CommandDefinition;
use crate::{
    cli::{
        client::{self},
        context::Context,
        recent,
        ui::{self, Fallback},
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    config::{
        Config,
        named_commands::{self, Commands},
        repo::ConfigLayers,
        resolve::Stack,
    },
    execution,
    model::Workspace,
    paths::CommandPaths,
    protocol::ConfigTarget,
};

pub(super) async fn list(ctx: &Context, workspace: Option<SpaceSelector>) -> Result<i32> {
    let global = Config::load(&ctx.paths)?;
    let layers = listing_layers(ctx, workspace).await?;
    let definitions = Stack::new(&global, &layers).named(|config| &config.commands);
    let definitions: Vec<_> = definitions
        .into_iter()
        .map(|(name, (argv, layer))| CommandDefinition { name, argv, layer })
        .collect();
    ctx.show(&definitions, |definitions| {
        for command in definitions {
            outln!(
                "{} = {} ({})",
                command.name,
                serde_json::to_string(&command.argv).expect("serialize command argv"),
                command.layer,
            );
        }
    })?;
    Ok(0)
}

/// An explicit workspace takes precedence over the current directory.
async fn listing_layers(ctx: &Context, workspace: Option<SpaceSelector>) -> Result<ConfigLayers> {
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
    let workspaces = running_workspaces(ctx).await?;
    let Some(workspace) = current_workspace(&workspaces)? else {
        return Ok(ConfigLayers::default());
    };
    workspace_layers(ctx, SpaceSelector::id(&workspace.id)).await
}

async fn workspace_layers(ctx: &Context, workspace: SpaceSelector) -> Result<ConfigLayers> {
    crate::client::workspaces::config_layers(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
        .await
}

/// The configuration that defines a command for one run.
enum Definition {
    /// The global configuration: the named or current workspace runs it, else
    /// the one picked.
    Global(Option<SpaceSelector>),
    /// The named or current workspace's configuration.
    Workspace(SpaceId),
    /// Only these other workspaces define it; the user picks one.
    Elsewhere(Vec<Workspace>),
}

/// Where `name` is defined for a run in the `named` or current workspace; an
/// unknown command when no configuration it could run from defines it.
async fn definition(ctx: &Context, name: &str, named: Option<SpaceSelector>) -> Result<Definition> {
    if defined_globally(ctx, name)? {
        return Ok(Definition::Global(named));
    }
    let workspaces = running_workspaces(ctx).await?;
    if let Some(workspace) = selected_workspace(ctx, &workspaces, named.clone()).await? {
        if !defines(ctx, &workspace.id, name).await? {
            return Err(unknown_command(name));
        }
        return Ok(Definition::Workspace(workspace.id.clone()));
    }
    let defining = defining_workspaces(ctx, workspaces, name).await;
    if defining.is_empty() {
        return Err(unknown_command(name));
    }
    // The command is unknown wherever the named workspace would have run it.
    if let Some(selector) = named {
        return Err(anyhow::anyhow!("unknown workspace: {selector}").context(unknown_command(name)));
    }
    if !ctx.interactive() {
        return Err(anyhow::Error::new(ui::NoCurrentWorkspace)
            .context("repository-only commands require a current or explicit workspace")
            .context(unknown_command(name)));
    }
    Ok(Definition::Elsewhere(defining))
}

fn defined_globally(ctx: &Context, name: &str) -> Result<bool> {
    Ok(Config::load(&ctx.paths)?
        .resolve(&ConfigLayers::default())?
        .commands
        .contains_key(name))
}

/// The daemon's workspaces; none without a running daemon.
async fn running_workspaces(ctx: &Context) -> Result<Vec<Workspace>> {
    match crate::client::workspaces::list(&ctx.paths).await {
        Err(error) if client::is_unreachable(&error) => Ok(Vec::new()),
        workspaces => workspaces,
    }
}

/// The workspace `named` selects, else the current one; `None` when it does
/// not exist or there is no current workspace.
async fn selected_workspace<'a>(
    ctx: &Context,
    workspaces: &'a [Workspace],
    named: Option<SpaceSelector>,
) -> Result<Option<&'a Workspace>> {
    let Some(selector) = named else {
        return current_workspace(workspaces);
    };
    let selector = match recent::is_previous(&selector) {
        true => SpaceSelector::Id(recent::previous(ctx).await?),
        false => selector,
    };
    let spaces = crate::client::model::Spaces {
        workspaces: workspaces.to_vec(),
        chats: Vec::new(),
    };
    let found = spaces.find(&selector);
    Ok(found.and_then(|space| workspaces.iter().find(|known| known.id == *space.id())))
}

/// The workspace of the current directory, else the caller's scope.
pub(in crate::cli) fn current_workspace(workspaces: &[Workspace]) -> Result<Option<&Workspace>> {
    let current = ui::current_directory()?;
    let context = WorkspaceContext::from_directory(workspaces, current.as_deref());
    Ok(context.resolve(
        None,
        crate::env::workspace_scoped(),
        ScopeOrder::AfterDirectory,
    ))
}

async fn defines(ctx: &Context, workspace: &SpaceId, name: &str) -> Result<bool> {
    let target = ConfigTarget::Workspace(SpaceSelector::id(workspace));
    Ok(crate::client::workspaces::settings(&ctx.paths, target)
        .await?
        .commands
        .contains_key(name))
}

/// Those of `workspaces` whose repository configuration defines `name`.
async fn defining_workspaces(
    ctx: &Context,
    workspaces: Vec<Workspace>,
    name: &str,
) -> Vec<Workspace> {
    let mut defining = Vec::new();
    for workspace in workspaces {
        // A workspace whose configuration cannot load does not define it.
        if defines(ctx, &workspace.id, name).await.unwrap_or(false) {
            defining.push(workspace);
        }
    }
    defining
}

pub(crate) async fn run(
    ctx: &Context,
    name: &str,
    workspace: Option<SpaceSelector>,
    args: Vec<OsString>,
) -> Result<i32> {
    let definition = definition(ctx, name, workspace).await?;
    run_defined(ctx, name, definition, args).await
}

/// Runs `name` in a workspace whose configuration `definition` found it in.
async fn run_defined(
    ctx: &Context,
    name: &str,
    definition: Definition,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = match definition {
        Definition::Global(named) => {
            ui::select_workspace(ctx, named, Fallback::CurrentDirectory).await?
        }
        Definition::Workspace(workspace) => SpaceSelector::Id(workspace),
        Definition::Elsewhere(defining) => {
            SpaceSelector::Id(ui::pick_workspace(ctx, defining, "workspace").await?)
        }
    };
    recent::record(ctx, &workspace).await;
    let settings =
        crate::client::workspaces::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
            .await?;
    let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace).await?;
    let command = expand(
        &ctx.paths,
        &settings.commands,
        name,
        &inspection.workspace,
        args,
    )
    .await?;
    execution::run_command(
        &ctx.paths,
        SpaceSelector::Id(inspection.workspace.id),
        command,
    )
    .await
}

async fn expand(
    paths: &CommandPaths,
    commands: &Commands,
    name: &str,
    workspace: &Workspace,
    args: Vec<OsString>,
) -> Result<Vec<OsString>> {
    named_commands::expand_with_fields(
        paths,
        commands,
        name,
        workspace,
        args,
        &[("{prompt}", OsStr::new(""))],
    )
    .await
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
    anyhow::anyhow!(
        "unknown command {name:?} in the current context; `shoal run` lists the configured commands"
    )
}
