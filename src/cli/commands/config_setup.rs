//! `shoal config setup`: a setup agent writes the global config in a chat
//! without a repository, which reaches every space and so reads how each
//! repository is set up.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::chat;
use crate::config_file::ConfigExt;
use crate::{
    cli::{args::service::ConfigSetupArgs, context::Context},
    config::{Config, modes, templates::Subject},
    protocol::ConfigTarget,
};

/// Start the setup chat, writing the packaged default config first when
/// there is none and the agent is settled.
pub(super) async fn start(ctx: &Context, args: ConfigSetupArgs) -> Result<i32> {
    // Chosen before the config is written, so a refused agent leaves none.
    let settings = crate::client::workspaces::settings(&ctx.paths, ConfigTarget::Global).await?;
    let agent = chat::chat_agent(ctx, &settings, args.agent)?;
    let model = args.model.into();
    chat::check_model(&settings, &agent, &model)?;
    let config = installed_config(ctx)?;
    let options = chat::Options {
        narrowing: Default::default(),
        agent: Some(agent),
        place: chat::ChatPlace::here(args.here),
        name: args.name,
        model,
        args: args.args,
        session: args.session,
        fork: false,
        restore: args.restore,
        handoff: args.handoff,
        parent: None,
    };
    chat::start_outside_repositories(ctx, topic(&config), options).await
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
    let directory = crate::cli::agents::writable_config_dir(&path)?;
    Ok(directory.join(path.file_name().context("the config has no file name")?))
}

/// The chat that sets up the global config `config`, reopened in a new tab
/// or restored by the same command.
pub(in crate::cli::commands) fn topic(config: &Path) -> chat::Topic {
    chat::Topic {
        relaunch: vec!["config".into(), "setup".into()],
        name: modes::SETUP.to_owned(),
        asks_name: false,
        mode: modes::SETUP.to_owned(),
        subject: Some(Subject::told(subject(config))),
        linked_issue: None,
        links: Vec::new(),
        workspace: None,
    }
}

/// What the setup agent of the global config `config` starts on: what the
/// file holds and what it is based on.
fn subject(config: &Path) -> String {
    let config = config.display();
    format!(
        "Set up Shoal's global config, `{config}`, for this machine. Write only that \
         file, never a repository's config: the `[machine]` settings that describe this \
         machine, such as `load`, `max_agents`, `max_jobs`, `keep_awake`, the agents of \
         each tool, `forges`, `git_profiles` and `simulators`, and the defaults every \
         repository inherits, such as `agent.default`, `git_profile`, `cleanup`, `pr`, \
         `merge`, `review` and `idle_stop`. Base them on the AI tools installed here, \
         the forge hosts and Git identities of the registered repositories, which \
         `shoal repo ls` lists, and those repositories' configs. When every repository \
         repeats a setting, tell the user instead of moving it.\n\n\
         Edit `{config}` in place and keep its comments: `shoal config set` is not \
         available to you. After `shoal daemon reload`, tell the user if the daemon \
         rejected the file."
    )
}

#[cfg(test)]
mod tests;
