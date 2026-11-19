//! `shoal config setup`: a setup agent writes the global config in a chat
//! without a repository, which reaches every space and so reads how each
//! repository is set up.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::start;
use crate::config_file::ConfigExt;
use crate::{
    cli::{args::service::ConfigSetupArgs, context::Context, ui},
    config::{Config, modes},
    protocol::ConfigTarget,
};
use shoal_protocol::launches::{Launch, Setup};
use shoal_protocol::space_names::SpaceName;

/// Start the setup chat, writing the packaged default config first when
/// there is none and the agent is settled. As chats do, it starts at once,
/// and unless `--here` in a focused place beside this terminal, whatever
/// the `focus` setting says, since the user works with it straight away.
/// Without a terminal, or from a scoped process, the daemon starts it
/// detached instead, as it starts any chat.
pub(super) async fn start(ctx: &Context, mut args: ConfigSetupArgs) -> Result<i32> {
    // Chosen before the config is written, so a refused agent leaves none.
    let settings = shoal_workers::settings::of(&ctx.paths, ConfigTarget::Global).await?;
    let agent = crate::cli::agents::start_agent(ctx, &settings, args.agent.take())?;
    let model = std::mem::take(&mut args.model).into();
    if let Some(agent) = &agent {
        start::check_model(&settings, agent, &model)?;
    }
    let config = installed_config(ctx)?;
    let launch = Launch {
        setup: Some(Setup::Config),
        name: Some(SpaceName::derive(modes::ModeName::SETUP.as_str(), "chat")),
        agent,
        model,
        prompt: Some(subject(&config)),
        task: start::launch_args(args.args)?,
        directory: ui::current_directory()?,
        env: Some(crate::terminal::start_environment()),
        ..Launch::default()
    };
    let options = start::StartOptions {
        here: args.here,
        focus: !start::leaves_caller(ctx, args.here),
        ..Default::default()
    };
    start::place(ctx, launch, false, &settings, &options).await
}

/// The global config file, in a directory that exists and is named as the
/// agent's working directories name it; the packaged default is written
/// first when the file does not exist, as `config install default` writes
/// it.
fn installed_config(ctx: &Context) -> Result<PathBuf> {
    let (path, created) = Config::install(&ctx.paths)?;
    if created {
        eprintln!("Installed config default at {}", path.display());
    }
    let directory = crate::agents::writable_config_dir(&path)?;
    Ok(directory.join(path.file_name().context("the config has no file name")?))
}

/// What the setup agent of the global config `config` starts on: which file
/// it writes and how it saves it.
fn subject(config: &Path) -> String {
    let config = config.display();
    format!(
        "Set up Shoal's global config, `{config}`, for this machine. Write only that \
         file, never a repository's config.\n\n\
         Edit `{config}` in place and keep its comments: `shoal config set` is not \
         available to you. After `shoal config reload`, tell the user if the daemon \
         rejected the file."
    )
}

#[cfg(test)]
mod tests;
