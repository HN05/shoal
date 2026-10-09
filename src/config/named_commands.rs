//! Configured shortcuts use the same workspace selection and wrapper as `exec`.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
};

use anyhow::{Context as _, Result, ensure};
use clap::{CommandFactory, Parser};
use serde::Serialize;

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        ui::{self, Fallback},
        workspace_context::{ScopeOrder, WorkspaceContext},
    },
    config::{
        Config,
        placeholders::render_os as render,
        repo::{ConfigLayer, ConfigLayers},
        resolve::Stack,
    },
    execution,
    model::{DiffBase, Workspace},
    paths::Paths,
    protocol::{ConfigTarget, Method},
};

pub type Commands = BTreeMap<String, Vec<String>>;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CommandDefinition {
    pub name: String,
    pub argv: Vec<String>,
    pub layer: ConfigLayer,
    pub bare_name: BareName,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BareName {
    Shorthand,
    BuiltIn,
}

pub fn defaults() -> Commands {
    crate::ai::default_commands()
}

pub fn validate(commands: &Commands) -> Result<()> {
    for (name, argv) in commands {
        crate::validate::lowercase_name("command", name)?;
        ensure!(
            argv.first()
                .is_some_and(|program| !program.trim().is_empty())
                && argv.iter().all(|arg| !arg.contains('\0')),
            "command {name:?} needs a nonempty executable and arguments without NUL bytes"
        );
        ensure!(
            argv[0] != "{args}" && argv.iter().filter(|arg| *arg == "{args}").count() <= 1,
            "command {name:?} may contain {{args}} once, after its executable"
        );
    }
    Ok(())
}

pub async fn list(ctx: &Context) -> Result<i32> {
    let global = Config::load(&ctx.paths)?;
    let mut layers = ConfigLayers::default();
    if client::status(&ctx.paths).await?.is_some() {
        let workspaces = client::workspaces(&ctx.paths).await?;
        let current = std::env::current_dir().ok();
        let context = WorkspaceContext::from_directory(&workspaces, current.as_deref());
        let workspace = context.resolve(None, crate::env::is_scoped(), ScopeOrder::AfterDirectory);
        if let Some(workspace) = workspace {
            layers = *request::<Box<ConfigLayers>>(
                &ctx.paths,
                Method::LayeredConfig {
                    target: ConfigTarget::Workspace(workspace.id.clone()),
                },
            )
            .await?;
        }
    }
    let definitions = Stack::new(&global, &layers).named(|config| &config.commands);

    let built_ins: BTreeSet<_> = crate::cli::Cli::command()
        .get_subcommands()
        .map(|command| command.get_name().to_owned())
        .collect();
    let definitions: Vec<_> = definitions
        .into_iter()
        .map(|(name, (argv, layer))| {
            let bare_name = if name == "help" || built_ins.contains(&name) {
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
            println!(
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

#[derive(Parser)]
struct Invocation {
    workspace: Option<String>,
    #[arg(last = true)]
    args: Vec<OsString>,
}

/// The name, workspace and arguments of `shoal <name>`; `None` once help
/// has been printed.
pub type Invoked = (String, Option<String>, Vec<OsString>);

pub fn invocation(words: Vec<OsString>) -> Result<Option<Invoked>> {
    let name = words[0]
        .to_str()
        .context("command name must be UTF-8")?
        .to_owned();
    match Invocation::try_parse_from(words) {
        Ok(invocation) => Ok(Some((name, invocation.workspace, invocation.args))),
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            error.print()?;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn run(
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
            ui::pick_workspace(ctx, defining)?
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

pub async fn expand(
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

pub async fn expand_with_fields(
    paths: &Paths,
    commands: &Commands,
    name: &str,
    workspace: &Workspace,
    args: Vec<OsString>,
    extra_fields: &[(&str, &OsStr)],
) -> Result<Vec<OsString>> {
    let argv = commands.get(name).ok_or_else(|| unknown_command(name))?;
    let base = if argv.iter().any(|arg| arg.contains("{diff_base}")) {
        Some(
            request::<DiffBase>(
                paths,
                Method::DiffBase {
                    workspace: workspace.id.clone(),
                },
            )
            .await?
            .commit,
        )
    } else {
        None
    };
    let mut fields = vec![
        ("{workspace}", OsStr::new(&workspace.name)),
        ("{branch}", OsStr::new(&workspace.branch)),
        ("{path}", workspace.path.as_os_str()),
    ];
    if let Some(base) = &base {
        fields.push(("{diff_base}", OsStr::new(base)));
    }
    fields.extend_from_slice(extra_fields);
    let mut command = Vec::new();
    let mut args = Some(args);
    for arg in argv {
        if arg == "{args}" {
            command.extend(args.take().unwrap_or_default());
        } else {
            command.push(render(arg, &fields));
        }
    }
    command.extend(args.unwrap_or_default());
    Ok(command)
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

#[cfg(test)]
mod tests {
    use crate::config::{self, Config};

    #[test]
    fn commands_layer_by_name_and_replace_whole_argument_arrays() {
        let global: Config =
            toml::from_str("[commands]\nreview = ['global', '--flag']\ncheck = ['check']\n")
                .unwrap();
        let layers = config::repo::ConfigLayers {
            worktree_file: config::repo::parse("[commands]\nreview = ['file']\n").unwrap(),
            saved_repository_config: config::repo::parse(
                "[commands]\nreview = ['saved', 'two words']\n",
            )
            .unwrap(),
        };
        let effective = global.resolve(&layers).unwrap();
        assert_eq!(effective.commands["review"], ["saved", "two words"]);
        assert_eq!(effective.commands["check"], ["check"]);
    }

    #[test]
    fn invalid_commands_are_rejected() {
        for config in [
            "[commands]\nreview = []",
            "[commands]\nreview = ['{args}']",
            "[commands]\nreview = ['tool', '{args}', '{args}']",
            "[commands]\nreview = ['']",
            "[commands]\nreview = ['   ']",
            "[commands]\nreview = ['tool', \"\\u0000\"]",
            "[commands]\n'bad name' = ['tool']",
            "[commands]\nreview = 'shell command'",
        ] {
            assert!(config::repo::parse(config).is_err(), "{config}");
        }
    }

    #[test]
    fn built_in_names_are_valid_for_explicit_run() {
        for name in ["run", "ls", "help"] {
            let config = format!("[commands]\n{name} = ['tool']\n");
            assert!(config::repo::parse(&config).is_ok(), "{config}");
        }
    }
}
