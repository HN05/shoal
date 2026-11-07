//! `shoal alerts`: what the daemon saw while the user was away, shown
//! once and then marked read unless `--keep-unread`; `--follow` keeps the
//! terminal on the stream and raises each entry as a terminal notification.
//! `shoal alerts send` adds one, `withdraw` takes back an unread one it added, and
//! `clear` deletes them all.
use std::io::{IsTerminal, Write};

use anyhow::{Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        client::{self},
        context::Context,
        installed::arguments_with,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::notifications::{self, Notification, Watch},
    installed::Executable,
};

pub(super) async fn list(ctx: &Context, all: bool, keep_unread: bool, limit: u32) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let notifications = notifications::list(&ctx.paths, !all, limit).await?;
    ctx.show(&notifications, |notifications| {
        let now = crate::time::UnixTime::now();
        let rows = notifications.iter().map(|n| cells(n, now)).collect();
        let lines = ui::aligned(rows, Palette::stdout(ctx.json));
        for (line, notification) in lines.iter().zip(notifications) {
            outln!("{}", with_files(line, notification));
        }
        if notifications.is_empty() {
            outln!("No {}notifications", if all { "" } else { "new " });
        }
    })?;
    // Shown once: exactly what this listing printed does not come back as new.
    if !keep_unread && !notifications.is_empty() {
        let ids = notifications.iter().map(|n| n.id).collect();
        notifications::mark_read(&ctx.paths, ids).await?;
    }
    if !all && let Some(status) = client::status(&ctx.paths).await? {
        // A listing kept unread still counts what it printed.
        let shown = if keep_unread { notifications.len() } else { 0 };
        let remaining = status.unread_notifications.saturating_sub(shown as u64);
        if remaining > 0 {
            eprintln!(
                "{remaining} more new notification{}; run shoal alerts again",
                if remaining == 1 { "" } else { "s" }
            );
        }
    }
    Ok(0)
}

pub(super) async fn clear(ctx: &Context) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let cleared = notifications::clear(&ctx.paths).await?;
    ctx.show(&cleared, |cleared| match cleared.cleared {
        0 => outln!("No notifications"),
        n => outln!("Cleared {n} notification{}", if n == 1 { "" } else { "s" }),
    })?;
    Ok(0)
}

/// The daemon refuses scoped requests; this also refuses a descendant that
/// cleared its scope token.
fn refuse_workspace_processes(ctx: &Context) -> Result<()> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot read or clear notifications; send one with shoal alerts send"
    );
    Ok(())
}

pub(super) async fn withdraw(ctx: &Context, space: Option<SpaceSelector>, id: i64) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let withdrawn = notifications::withdraw(&ctx.paths, SpaceSelector::parse(&space), id).await?;
    ctx.emit(
        &format!("Withdrew notification {id}: {}", withdrawn.message),
        &withdrawn,
    )?;
    Ok(0)
}

pub(super) async fn send(
    ctx: &Context,
    space: Option<SpaceSelector>,
    message: String,
    attach: Vec<String>,
) -> Result<i32> {
    let attachments = super::messages::attachments(&attach)?;
    crate::client::files::check_sent(&attachments)?;
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let sender_chat = crate::env::chat();
    let sent = notifications::send(
        &ctx.paths,
        SpaceSelector::parse(&space),
        message,
        attachments,
        sender_chat,
    )
    .await?;
    let text = match sent.notification {
        Some(id) => format!("Notification {id} sent."),
        None => "Sent to the coordinating agent.".to_owned(),
    };
    ctx.emit(&text, &sent)?;
    Ok(0)
}

/// The daemon marks each notification read as it delivers it unless
/// `keep_unread`; the stream resumes after the last one printed. On a
/// terminal, each one is also raised as a terminal notification so the
/// emulator can show it while another window has focus.
pub(super) async fn follow(ctx: &Context, keep_unread: bool, after: i64) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let executable = Executable::current();
    let mut watch = Watch::start(&ctx.paths, keep_unread, after).await?;
    let palette = Palette::stdout(ctx.json);
    let raise = !ctx.json && std::io::stdout().is_terminal();
    loop {
        let notification = watch.next(&ctx.paths).await.map_err(|error| {
            let delivered = watch.delivered();
            executable.resume_or(error, || arguments_with("--resume", Some(delivered)))
        })?;
        ctx.show(&notification, |notification| {
            outln!(
                "{}",
                render(notification, palette, crate::time::UnixTime::now())
            );
        })?;
        if raise {
            crate::stdout::write(|stdout| {
                stdout.write_all(terminal_notification(&notification).as_bytes())?;
                stdout.flush()
            })?;
        }
    }
}

/// OSC 9 desktop notification, shown by iTerm2, Ghostty, WezTerm, and Kitty
/// and ignored by terminals without it. Control characters would end or
/// corrupt the sequence, so they are dropped from the text.
fn terminal_notification(notification: &Notification) -> String {
    let text: String = format!(
        "Shoal {}: {}",
        notification.workspace.as_deref().unwrap_or("daemon"),
        notification.message
    )
    .chars()
    .filter(|c| !c.is_control())
    .collect();
    format!("\x1b]9;{text}\x07")
}

fn render(notification: &Notification, palette: Palette, now: crate::time::UnixTime) -> String {
    let line = ui::aligned(vec![cells(notification, now)], palette).concat();
    with_files(&line, notification)
}

/// The notification's row `line`, then a line for each file its space sent
/// with it.
fn with_files(line: &str, notification: &Notification) -> String {
    std::iter::once(line.to_owned())
        .chain(
            (notification.files.iter())
                .map(|file| format!("  Attached file: {}", file.path.display())),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

/// When, read at `now`, the workspace, and the message.
fn cells(notification: &Notification, now: crate::time::UnixTime) -> [ui::Cell; 3] {
    [
        (notification.created_at.local_ago(now), Some(Style::Muted)),
        (
            notification.workspace.clone().unwrap_or_else(|| "-".into()),
            Some(Style::Heading),
        ),
        (notification.message.clone(), None),
    ]
}

#[cfg(test)]
mod tests;
