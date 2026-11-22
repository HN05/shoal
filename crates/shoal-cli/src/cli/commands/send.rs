//! `shoal send` sends an alert: from a space up to its coordinator, or else
//! the user; with `--space` down to that space's agent, from the user's
//! terminal to a picked one; with `--all` to every agent the sender's scope
//! reaches, running, stopped or queued, or with `--repo` to those of one
//! repository, the current one without a value. It lists where it went.
//! Led by a thread's number, it posts in that thread instead (see
//! [`super::threads`]). With `--in-chat`, the agent's turn's final message
//! is the text, sent once the turn ends.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};

use crate::cli::args::threads::PostArgs;
use crate::{
    cli::{
        context::Context,
        ui::{self, Acting, Fallback},
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
    threads::ThreadNumber,
};

/// What `shoal send`'s arguments say: an alert's text, or a post in a
/// thread, or either from the agent's chat.
pub(super) enum Sending {
    Alert(Option<String>),
    /// The turn's final message as an alert.
    AlertInChat,
    Post {
        thread: ThreadNumber,
        text: Option<String>,
    },
    /// The turn's final message as a post in this thread.
    PostInChat(ThreadNumber),
}

/// Which of `shoal send`'s options other than a post's are given.
pub(super) struct Given {
    pub body_file: bool,
    pub in_chat: bool,
    pub attached: bool,
    /// Whether an alert's recipients are named.
    pub recipients: bool,
}

/// What `args` say, given the options only posts take and the others
/// `given`: a thread's number and the text post in that thread, as does a
/// single argument with `--body-file` or such an option, or with
/// `--in-chat` the turn's final message; any other single argument is an
/// alert's text, and `--in-chat` alone makes the final message one. A post
/// goes to the thread's other side, so it takes no recipients, and one from
/// the chat carries no files.
pub(super) fn sending(args: Vec<String>, post: &PostArgs, given: Given) -> Result<Sending> {
    let sending = sending_of(args, post, &given)?;
    let posts = matches!(sending, Sending::Post { .. } | Sending::PostInChat(_));
    if given.recipients && posts {
        bail!("a post goes to its thread's other side; --space, --all and --repo name an alert's");
    }
    if given.attached && matches!(sending, Sending::PostInChat(_)) {
        bail!(
            "a post from your chat carries no files; post them with shoal send <thread> --attach"
        );
    }
    Ok(sending)
}

/// What `args` say, as [`sending`] reads them, whatever recipients and
/// files are given.
fn sending_of(args: Vec<String>, post: &PostArgs, given: &Given) -> Result<Sending> {
    let mut args = args.into_iter();
    Ok(match (args.next(), args.next()) {
        (_, Some(_)) if given.in_chat => {
            bail!("--in-chat takes no text: your turn's final message is the text")
        }
        (Some(thread), None) if given.in_chat => Sending::PostInChat(thread.parse()?),
        (None, None) if given.in_chat => Sending::AlertInChat,
        (Some(thread), Some(text)) => Sending::Post {
            thread: thread.parse()?,
            text: Some(text),
        },
        (Some(thread), None) if given.body_file || post.given() => Sending::Post {
            thread: thread.parse()?,
            text: None,
        },
        (text, _) if !post.given() => Sending::Alert(text),
        _ => bail!("name the thread to post in: shoal send <thread> \"<text>\""),
    })
}

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
    let from = super::alerts::sending_space(ctx).await?;
    let sending = {
        let (from, message, attachments) = (from.clone(), message.clone(), attachments.clone());
        Acting::request(move |space| {
            let to = AlertTarget::Space { space };
            alerts::send(to, from.clone(), message.clone(), attachments.clone())
        })
    };
    let to = target(ctx, recipients, &sending).await?;
    let sent = (alerts::send(to, from, message, attachments))
        .send(&ctx.paths)
        .await?;
    show_sent(ctx, &sent)
}

/// Mark the agent's turn as sending its final message to `recipients`
/// with the files `attach` names, once the turn ends; they are checked
/// now and copied then.
pub(super) async fn send_in_chat(
    ctx: &Context,
    recipients: Recipients,
    attach: Vec<String>,
) -> Result<i32> {
    super::alerts::refuse_workspace_processes(ctx)?;
    // Only an agent's turn has a final message, so the user's terminal is
    // refused before it picks a recipient.
    ensure!(
        crate::env::is_scoped(),
        "only an agent Shoal runs sends its turn's final message; give the text to shoal send instead"
    );
    let attachments = attachments(&attach)?;
    shoal_protocol::files::check_sent(&attachments)?;
    let marking = {
        let attachments = attachments.clone();
        Acting::request(move |space| {
            alerts::send_in_chat(AlertTarget::Space { space }, attachments.clone())
        })
    };
    let to = target(ctx, recipients, &marking).await?;
    let marked = (alerts::send_in_chat(to, attachments))
        .send(&ctx.paths)
        .await?;
    in_chat::show(ctx, &marked)
}

/// Where `recipients` sends: a process in a space of its own sends up
/// without a space; a picker offers the spaces `acting`, what the alert
/// sends to the picked one, reaches.
async fn target(ctx: &Context, recipients: Recipients, acting: &Acting) -> Result<AlertTarget> {
    Ok(match recipients {
        Recipients::Space(None) if super::alerts::in_own_space() => AlertTarget::Up,
        Recipients::Space(space) => AlertTarget::Space {
            space: ui::select_space(ctx, space, Fallback::CurrentDirectory(acting)).await?,
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

pub(in crate::cli) mod in_chat;

#[cfg(test)]
mod tests;
