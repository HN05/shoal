//! `shoal send` sends an alert: from a space up to its coordinator, or else
//! the user; with `--space` down to that space's agent, from the user's
//! terminal to a picked one; with `--all` to every agent the sender's scope
//! reaches, running, stopped or queued, or with `--repo` to those of one
//! repository, the current one without a value. It lists where it went.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    client::alerts,
    forge::items::attach::Attachment,
    repository::Selector,
};
use shoal_protocol::{
    alerts::{ReachedSpaces, Refused, Sent},
    files::NewFile,
    protocol::AlertTarget,
    space_names::SpaceSelector,
};

/// Whom `shoal send` reaches.
pub(super) enum Recipients {
    /// The agent of the named space, else the coordinator of the caller's
    /// own, or for the user's terminal the current or a picked space's.
    Space(Option<SpaceSelector>),
    /// Every agent the sender's scope reaches.
    All,
    /// Every agent of the named repository's workspaces and chats, else the
    /// current one's or a picked one's.
    Repository(Option<String>),
}

pub(super) async fn send(
    ctx: &Context,
    recipients: Recipients,
    message: Option<String>,
    body_file: Option<PathBuf>,
    attach: Vec<String>,
) -> Result<i32> {
    let message = message_text(message, body_file, !attach.is_empty())?;
    let attachments = attachments(&attach)?;
    let to = target(ctx, recipients).await?;
    let from = super::alerts::sending_space(ctx).await?;
    let sent = alerts::send(&ctx.paths, to, from, message, attachments).await?;
    show_sent(ctx, &sent)
}

/// Where `recipients` sends: a process in a space of its own sends up
/// without a space.
async fn target(ctx: &Context, recipients: Recipients) -> Result<AlertTarget> {
    Ok(match recipients {
        Recipients::Space(None) if super::alerts::in_own_space() => AlertTarget::Up,
        Recipients::Space(space) => AlertTarget::Space {
            space: ui::select_space(ctx, space, Fallback::CurrentDirectory).await?,
        },
        Recipients::All => AlertTarget::All { repository: None },
        Recipients::Repository(repository) => AlertTarget::All {
            repository: Some(chosen_repository(ctx, repository).await?),
        },
    })
}

/// Where an alert went, exiting 1 when a space refused it.
fn show_sent(ctx: &Context, sent: &Sent) -> Result<i32> {
    ctx.show(sent, |sent| {
        let reached = &sent.reached;
        if let [id] = sent.alerts.as_slice() {
            outln!("Alert {id} sent.");
        }
        if sent.user {
            outln!("It went to the user.");
        } else if sent.passed_up {
            outln!("Passed up to the coordinating agent on another machine.");
        } else if !sent.alerts.is_empty() && reached.is_empty() {
            outln!("It went to the coordinating agent.");
        } else if reached.is_empty() {
            outln!("No workspace or chat has an agent.");
        }
        show_reached("Sent to:", &reached.now);
        show_reached("Queued for when their agent next runs:", &reached.on_start);
        show_reached("Queued until an agent starts in:", &reached.without_agent);
        show_refused(&reached.refused);
    })?;
    Ok(if sent.reached.refused.is_empty() {
        0
    } else {
        1
    })
}

/// The spaces under `heading`, unless there are none.
fn show_reached(heading: &str, spaces: &ReachedSpaces) {
    if spaces.is_empty() {
        return;
    }
    outln!("{heading}");
    for workspace in &spaces.workspaces {
        outln!("  {}", workspace.label());
    }
    for chat in &spaces.chats {
        outln!("  {} (chat)", chat.label());
    }
}

/// The spaces that refused the alert, each with why, unless there are none.
fn show_refused(refused: &Refused) {
    if refused.is_empty() {
        return;
    }
    outln!("Not sent to:");
    for workspace in &refused.workspaces {
        outln!("  {}: {}", workspace.space.label(), workspace.error);
    }
    for chat in &refused.chats {
        outln!("  {} (chat): {}", chat.space.label(), chat.error);
    }
}

/// The repository `--repo` names, else the current checkout's or
/// workspace's, else a picked one.
async fn chosen_repository(ctx: &Context, repository: Option<String>) -> Result<Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = crate::client::repositories::list(&ctx.paths).await?;
            Ok(
                super::issues::current_repository(ctx, repos, "pass --repo <repository>")
                    .await?
                    .into(),
            )
        }
    }
}

/// The files to attach, given as `PATH` or `PATH#TITLE`, by the absolute
/// paths the daemon reads, each checked before a space is picked.
pub(super) fn attachments(args: &[String]) -> Result<Vec<NewFile>> {
    args.iter()
        .map(|arg| {
            let given = Attachment::parse(arg, Path::new(""));
            let path = std::path::absolute(&given.path)
                .with_context(|| format!("read {}", given.path.display()))?;
            shoal_protocol::files::check_path(&path)?;
            let file = NewFile {
                path,
                title: given.alt,
            };
            file.check()?;
            Ok(file)
        })
        .collect()
}

/// The text given inline or read from `body_file`, without the file's
/// trailing newline; empty when the `attached` files are the alert.
fn message_text(
    message: Option<String>,
    body_file: Option<PathBuf>,
    attached: bool,
) -> Result<String> {
    match body_file {
        Some(path) => Ok(std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?
            .trim_end()
            .to_owned()),
        None => message
            .or_else(|| attached.then(String::new))
            .context("give the text, --body-file or --attach"),
    }
}

#[cfg(test)]
mod tests;
