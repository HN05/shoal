//! `shoal alerts`: what the daemon saw while the user was away, shown
//! once and then marked read unless `--keep-unread`; `--follow` keeps the
//! terminal on the stream and raises each entry as a terminal notification.
//! `shoal alerts send` adds one, `withdraw` takes back an unread one it added, and
//! `clear` deletes them all.
use std::io::{IsTerminal, Write};

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        installed::{Executable, arguments_with},
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::notifications::{Cleared, Notification, Sent},
    protocol::{self, Method, Response},
};

pub(super) async fn list(ctx: &Context, all: bool, keep_unread: bool, limit: u32) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let method = Method::ListNotifications {
        unread_only: !all,
        limit,
    };
    let notifications = request::<Vec<Notification>>(&ctx.paths, method).await?;
    ctx.show(&notifications, |notifications| {
        let now = crate::time::unix_seconds() as i64;
        let rows = notifications.iter().map(|n| cells(n, now)).collect();
        for line in ui::aligned(rows, Palette::stdout(ctx.json)) {
            outln!("{line}");
        }
        if notifications.is_empty() {
            outln!("No {}notifications", if all { "" } else { "new " });
        }
    })?;
    // Shown once: exactly what this listing printed does not come back as new.
    if !keep_unread && !notifications.is_empty() {
        let ids = notifications.iter().map(|n| n.id).collect();
        request::<()>(&ctx.paths, Method::MarkNotificationsRead { ids }).await?;
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
    let cleared = request::<Cleared>(&ctx.paths, Method::ClearNotifications).await?;
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

pub(super) async fn withdraw(ctx: &Context, workspace: Option<String>, id: i64) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let withdrawn =
        request::<Notification>(&ctx.paths, Method::WithdrawMessage { workspace, id }).await?;
    ctx.emit(
        &format!("Withdrew notification {id}: {}", withdrawn.message),
        &withdrawn,
    )?;
    Ok(0)
}

pub(super) async fn send(ctx: &Context, workspace: Option<String>, message: String) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let sent = request::<Sent>(&ctx.paths, Method::SendMessage { workspace, message }).await?;
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
    let mut delivered = after;
    let watch = |after| Method::WatchNotifications { keep_unread, after };
    let (mut stream, body) = client::open_buffered(&ctx.paths, watch(delivered)).await?;
    <()>::try_from(body)?;
    let palette = Palette::stdout(ctx.json);
    let raise = !ctx.json && std::io::stdout().is_terminal();
    loop {
        let response: Response = match protocol::read_buffered(&mut stream).await {
            Ok(response) => response,
            Err(error) if client::stream_closed(&error) => {
                let (next, body) = client::reconnect(&ctx.paths, || watch(delivered))
                    .await
                    .map_err(|error| {
                        executable.resume_or(error, || arguments_with("--resume", Some(delivered)))
                    })?;
                <()>::try_from(body)?;
                stream = next;
                continue;
            }
            Err(error) => return Err(error).context("daemon closed the notification stream"),
        };
        let notification = Notification::try_from(response.body)?;
        delivered = notification.id;
        ctx.show(&notification, |notification| {
            outln!(
                "{}",
                render(notification, palette, crate::time::unix_seconds() as i64)
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

fn render(notification: &Notification, palette: Palette, now: i64) -> String {
    ui::aligned(vec![cells(notification, now)], palette).concat()
}

/// When, read at `now`, the workspace, and the message.
fn cells(notification: &Notification, now: i64) -> [ui::Cell; 3] {
    [
        (
            crate::time::local_ago(notification.created_at, now),
            Some(Style::Muted),
        ),
        (
            notification.workspace.clone().unwrap_or_else(|| "-".into()),
            Some(Style::Heading),
        ),
        (notification.message.clone(), None),
    ]
}

#[cfg(test)]
mod tests;
