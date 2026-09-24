//! Configured shortcuts use the same workspace selection and wrapper as `exec`.
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
};

use anyhow::{Context as _, Result, ensure};
use clap::{CommandFactory, Parser};
use serde::{Deserialize, Serialize};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        ui::{self, Fallback},
    },
    config::{Config, repo::ConfigLayer},
    execution,
    model::{DiffBase, Workspace},
    paths::Paths,
    protocol::{ConfigTarget, Method},
};

pub type Commands = BTreeMap<String, Vec<String>>;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CommandLayers {
    pub worktree_file: Commands,
    pub saved_repository_config: Commands,
}

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
    [
        (
            "claude",
            vec!["claude", "{args}", "--remote-control", "{workspace}"],
        ),
        (
            "codex",
            vec![
                "codex",
                "{args}",
                "--sandbox",
                "danger-full-access",
                "--ask-for-approval=never",
            ],
        ),
    ]
    .into_iter()
    .map(|(name, argv)| (name.into(), argv.into_iter().map(String::from).collect()))
    .collect()
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
    let mut definitions: BTreeMap<String, (Vec<String>, ConfigLayer)> = defaults()
        .into_iter()
        .map(|(name, argv)| (name, (argv, ConfigLayer::BuiltInDefault)))
        .collect();
    definitions.extend(
        global
            .commands
            .into_iter()
            .map(|(name, argv)| (name, (argv, ConfigLayer::GlobalConfig))),
    );

    if client::status(&ctx.paths).await?.is_some() {
        let workspaces = client::workspaces(&ctx.paths).await?;
        let current = std::env::current_dir().ok();
        let workspace = current
            .as_deref()
            .and_then(|cwd| Workspace::innermost(&workspaces, cwd))
            .or_else(|| {
                crate::env::is_scoped()
                    .then(|| workspaces.first())
                    .flatten()
            });
        if let Some(workspace) = workspace {
            let layers = request::<CommandLayers>(
                &ctx.paths,
                Method::CommandLayers {
                    workspace: workspace.id.clone(),
                },
            )
            .await?;
            definitions.extend(
                layers
                    .worktree_file
                    .into_iter()
                    .map(|(name, argv)| (name, (argv, ConfigLayer::WorktreeFile))),
            );
            definitions.extend(
                layers
                    .saved_repository_config
                    .into_iter()
                    .map(|(name, argv)| (name, (argv, ConfigLayer::SavedRepositoryConfig))),
            );
        }
    }

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

pub async fn invoke(ctx: &Context, words: Vec<OsString>) -> Result<i32> {
    let name = words[0]
        .to_str()
        .context("command name must be UTF-8")?
        .to_owned();
    let invocation = match Invocation::try_parse_from(words) {
        Ok(invocation) => invocation,
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            error.print()?;
            return Ok(0);
        }
        Err(error) => return Err(error.into()),
    };
    run(ctx, &name, invocation.workspace, invocation.args).await
}

pub async fn run(
    ctx: &Context,
    name: &str,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let global = Config::load(&ctx.paths)?;
    let known_globally = global.commands.contains_key(name) || defaults().contains_key(name);
    let fallback = if known_globally {
        Fallback::CurrentDirectory
    } else {
        Fallback::CurrentDirectoryOnly
    };
    let workspace = ui::select_workspace(ctx, workspace, fallback).await.with_context(|| {
        format!("command {name:?} needs a workspace; repository-only commands require a current or explicit workspace")
    })?;
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
    execution::run(&ctx.paths, workspace, command, None).await
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
    let argv = commands.get(name).with_context(|| {
        format!("unknown command {name:?}; define it in [commands] in Shoal config")
    })?;
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

/// Substitute once, preserving non-UTF-8 paths and literal inserted values.
fn render(template: &str, fields: &[(&str, &OsStr)]) -> OsString {
    let mut output = OsString::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push(&rest[..start]);
        rest = &rest[start..];
        if let Some((key, value)) = fields.iter().find(|(key, _)| rest.starts_with(key)) {
            output.push(value);
            rest = &rest[key.len()..];
        } else {
            output.push("{");
            rest = &rest[1..];
        }
    }
    output.push(rest);
    output
}

#[cfg(test)]
mod tests {
    use crate::config::{self, Config};

    #[test]
    fn commands_layer_by_name_and_replace_whole_argument_arrays() {
        let global: Config =
            toml::from_str("[commands]\nreview = ['global', '--flag']\ncheck = ['check']\n")
                .unwrap();
        let file = config::repo::parse("[commands]\nreview = ['file']\n").unwrap();
        let saved = config::repo::parse("[commands]\nreview = ['saved', 'two words']\n").unwrap();
        let effective = global.effective(&saved.over(file)).unwrap();
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
        for name in ["run", "list", "help"] {
            let config = format!("[commands]\n{name} = ['tool']\n");
            assert!(config::repo::parse(&config).is_ok(), "{config}");
        }
    }
}
