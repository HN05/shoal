//! `shoal notifications`: what the daemon saw while the user was away, shown
//! once and then marked read; `--follow` keeps the terminal on the stream and
//! raises each entry as a terminal notification. `shoal notify` adds one.
use std::io::{IsTerminal, Write};

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::notifications::Notification,
    protocol::{self, Method, Response},
};

pub(super) async fn run(ctx: &Context, all: bool, follow: bool, limit: u32) -> Result<i32> {
    if follow {
        return follow_stream(ctx).await;
    }
    let method = Method::ListNotifications {
        unread_only: !all,
        limit,
    };
    let notifications = request::<Vec<Notification>>(&ctx.paths, method).await?;
    ctx.show(&notifications, |notifications| {
        let palette = Palette::stdout(ctx.json);
        for notification in notifications {
            println!("{}", render(notification, palette));
        }
        if notifications.is_empty() {
            println!("No {}notifications", if all { "" } else { "new " });
        }
    })?;
    // Shown once: exactly what this listing printed does not come back as new.
    if !notifications.is_empty() {
        let ids = notifications.iter().map(|n| n.id).collect();
        request::<()>(&ctx.paths, Method::MarkNotificationsRead { ids }).await?;
    }
    if !all
        && let Some(status) = client::status(&ctx.paths).await?
        && status.unread_notifications > 0
    {
        eprintln!(
            "{} more new notification{}; run shoal notifications again",
            status.unread_notifications,
            if status.unread_notifications == 1 {
                ""
            } else {
                "s"
            }
        );
    }
    Ok(0)
}

pub(super) async fn send(ctx: &Context, workspace: Option<String>, message: String) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    request::<()>(&ctx.paths, Method::SendMessage { workspace, message }).await?;
    ctx.emit("Notification sent.", ())?;
    Ok(0)
}

/// The daemon marks each notification read as it delivers it. On a terminal,
/// each one is also raised as a terminal notification so the emulator can show
/// it while another window has focus.
async fn follow_stream(ctx: &Context) -> Result<i32> {
    let (mut stream, body) = client::open_buffered(&ctx.paths, Method::WatchNotifications).await?;
    <()>::try_from(body)?;
    let palette = Palette::stdout(ctx.json);
    let raise = !ctx.json && std::io::stdout().is_terminal();
    loop {
        let response: Response = match protocol::read_buffered(&mut stream).await {
            Ok(response) => response,
            Err(error) if client::stream_closed(&error) => {
                let (next, body) =
                    client::reconnect(&ctx.paths, || Method::WatchNotifications).await?;
                <()>::try_from(body)?;
                stream = next;
                continue;
            }
            Err(error) => return Err(error).context("daemon closed the notification stream"),
        };
        let notification = Notification::try_from(response.body)?;
        ctx.show(&notification, |notification| {
            println!("{}", render(notification, palette));
        })?;
        if raise {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(terminal_notification(&notification).as_bytes())?;
            stdout.flush()?;
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

fn render(notification: &Notification, palette: Palette) -> String {
    format!(
        "{}  {}  {}",
        palette.paint(
            Style::Muted,
            crate::time::local_minutes(notification.created_at)
        ),
        palette.paint(
            Style::Heading,
            notification.workspace.as_deref().unwrap_or("-")
        ),
        notification.message
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn terminal_notification_is_one_osc_9_sequence_without_control_characters() {
        let notification = crate::daemon::notifications::Notification {
            id: 1,
            created_at: 0,
            workspace: Some("fix-login".into()),
            kind: crate::daemon::notifications::NotificationKind::AgentExited,
            message: "claude exited\x07 with code 0\n".into(),
            read: false,
        };
        assert_eq!(
            super::terminal_notification(&notification),
            "\x1b]9;Shoal fix-login: claude exited with code 0\x07"
        );
    }
}
