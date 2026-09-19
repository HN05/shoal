//! Configured shortcuts use the same workspace selection and wrapper as `exec`.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::{Context as _, Result, ensure};
use clap::{CommandFactory, Parser};

use crate::{
    client,
    context::Context,
    execution,
    protocol::ConfigTarget,
    ui::{self, Fallback},
};

pub type Commands = BTreeMap<String, Vec<String>>;

pub fn validate(commands: &Commands) -> Result<()> {
    let cli = crate::cli::Cli::command();
    for (name, argv) in commands {
        crate::validate::lowercase_name("command", name)?;
        ensure!(
            !cli.get_subcommands()
                .any(|command| command.get_name() == name)
                && name != "help",
            "command {name:?} conflicts with a built-in Shoal command"
        );
        ensure!(
            argv.first()
                .is_some_and(|program| !program.trim().is_empty())
                && argv.iter().all(|arg| !arg.contains('\0')),
            "command {name:?} needs a nonempty executable and arguments without NUL bytes"
        );
    }
    Ok(())
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
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    let argv = settings.commands.get(name).with_context(|| {
        format!("unknown command {name:?}; define it in [commands] in Shoal config")
    })?;
    let command = argv.iter().map(OsString::from).chain(args).collect();
    execution::run(&ctx.paths, workspace, command, None).await
}

#[cfg(test)]
mod tests {
    use crate::{config::Config, repo_config};

    #[test]
    fn commands_layer_by_name_and_replace_whole_argument_arrays() {
        let global: Config =
            toml::from_str("[commands]\nreview = ['global', '--flag']\ncheck = ['check']\n")
                .unwrap();
        let file = repo_config::parse("[commands]\nreview = ['file']\n").unwrap();
        let saved = repo_config::parse("[commands]\nreview = ['saved', 'two words']\n").unwrap();
        let effective = global.effective(&saved.over(file)).unwrap();
        assert_eq!(effective.commands["review"], ["saved", "two words"]);
        assert_eq!(effective.commands["check"], ["check"]);
    }

    #[test]
    fn invalid_commands_are_rejected() {
        for config in [
            "[commands]\nreview = []",
            "[commands]\nreview = ['']",
            "[commands]\nreview = ['   ']",
            "[commands]\nreview = ['tool', \"\\u0000\"]",
            "[commands]\nrm = ['tool']",
            "[commands]\nhelp = ['tool']",
            "[commands]\n'bad name' = ['tool']",
            "[commands]\nreview = 'shell command'",
        ] {
            assert!(repo_config::parse(config).is_err(), "{config}");
        }
    }
}
