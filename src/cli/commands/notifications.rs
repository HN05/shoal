//! `shoal notifications`: what the daemon saw while the user was away, shown
//! once and then marked read; `--follow` keeps the terminal on the stream and
//! raises each entry as a terminal notification.
use std::io::{IsTerminal, Write};

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::{Palette, Style},
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

/// The daemon marks each notification read as it delivers it. On a terminal,
/// each one is also raised as a terminal notification so the emulator can show
/// it while another window has focus.
async fn follow_stream(ctx: &Context) -> Result<i32> {
    let (mut stream, body) = client::open(&ctx.paths, Method::WatchNotifications).await?;
    <()>::try_from(body)?;
    let palette = Palette::stdout(ctx.json);
    let raise = !ctx.json && std::io::stdout().is_terminal();
    loop {
        let response: Response = protocol::read(&mut stream)
            .await
            .context("daemon closed the notification stream")?;
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
        palette.paint(Style::Muted, local_time(notification.created_at)),
        palette.paint(
            Style::Heading,
            notification.workspace.as_deref().unwrap_or("-")
        ),
        notification.message
    )
}

/// `YYYY-MM-DD HH:MM` in the local time zone.
fn local_time(unix_seconds: i64) -> String {
    let time: libc::time_t = unix_seconds as libc::time_t;
    // SAFETY: localtime_r writes only into the zeroed `tm` passed to it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return unix_seconds.to_string();
        }
        tm
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
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

    #[test]
    fn local_time_formats_the_epoch_in_the_process_time_zone() {
        // Only the shape is stable across zones; the value is checked at UTC.
        let text = super::local_time(1_700_000_000);
        assert_eq!(text.len(), 16, "{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
    }
}
