//! Local setup: the agent saves the repository's local config and the
//! scripts it runs in a chat at the registered checkout, leaving the
//! repository's Git tree untouched.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        commands::{chat, repositories},
        context::Context,
    },
    config::Effective,
    model::Repository,
    repository::{self, Selector},
};

/// The branch the setup script is tried on, in a workspace the agent removes.
const TRIAL_BRANCH: &str = "shoal-setup-trial";

/// Start the chat about `repo`, whose `settings` chose the agent `options`
/// name, if any.
pub(super) async fn start(
    ctx: &Context,
    repo: Repository,
    settings: Effective,
    options: chat::Options,
) -> Result<i32> {
    chat::ensure_unscoped(ctx)?;
    options.check_name(ctx).await?;
    let config = config_file(ctx, repo.id.clone().into()).await?;
    let topic = topic(&repo, &config);
    chat::begin(ctx, repo, settings, topic, options).await
}

/// The repository's local config file, in a directory that exists and is
/// named as agents compare paths, so the chat may write there.
async fn config_file(ctx: &Context, selector: Selector) -> Result<PathBuf> {
    let path = repositories::local_config_path(ctx, selector).await?;
    let directory = path.parent().context("the local config has no directory")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("create {}", directory.display()))?;
    let directory = std::fs::canonicalize(directory)
        .with_context(|| format!("resolve {}", directory.display()))?;
    Ok(directory.join(
        path.file_name()
            .context("the local config has no file name")?,
    ))
}

/// A chat that saves `repo`'s local config `config`, which it may write
/// beside, reopened in a new tab by the same command.
fn topic(repo: &Repository, config: &Path) -> chat::Topic {
    chat::Topic {
        relaunch: vec![
            "repo".into(),
            "setup".into(),
            repo.id.clone().into(),
            "--local".into(),
        ],
        name: format!("{}-setup", repository::name(repo)),
        asks_name: false,
        opening: Some(prompt(repository::name(repo), config)),
        directories: config.parent().map(Path::to_path_buf).into_iter().collect(),
    }
}

/// The setup agent's instructions for repository `name`, whose local config
/// is `config`.
pub(super) fn prompt(name: &str, config: &Path) -> String {
    let directory = config.parent().unwrap_or(config).display();
    super::prompt(
        name,
        "this chat runs in its registered checkout",
        &format!(
            "its local config, `{}`, which Shoal reads in place outside the repository's \
             Git tree,",
            config.display()
        ),
        &format!(
            "Save the scripts the config runs in `{directory}` beside it and name them by \
             paths relative to that directory; `setup.seed` stays relative to the worktree. \
             The repository's own `.shoal.toml` or `.shoal/config.toml`, when it has one, \
             still applies beneath the local config option by option, so write only what it \
             lacks or must change. Leave the checkout's files and branches unchanged and \
             commit nothing: the local config applies to every workspace of the repository \
             as soon as you save it.\n\n\
             Check the result with `shoal config show --repo {name}`. Try the setup script in a \
             temporary workspace: `shoal add {name} {TRIAL_BRANCH}` creates one and runs it, \
             and `shoal work setup <workspace>` runs it again after a change. Then remove that \
             workspace and its branch with `shoal rm <workspace> -y`, and tell the user to \
             check the config with `shoal config show --repo {name}`."
        ),
    )
}
