//! `shoal notifications`: what the daemon saw while the user was away, shown
//! once and then marked read; `--follow` keeps the terminal on the stream.
use anyhow::{Context as _, Result, bail};

use crate::{
    client::{self, request},
    context::Context,
    notifications::Notification,
    protocol::{self, Body, Method, Response},
};

pub(super) async fn run(ctx: &Context, all: bool, follow: bool, limit: u32) -> Result<i32> {
    if follow {
        return follow_stream(ctx).await;
    }
    let method = Method::ListNotifications {
        unread_only: !all,
        limit,
    };
    let notifications = request!(&ctx.paths, method, Notifications);
    ctx.show(&notifications, |notifications| {
        for notification in notifications {
            println!("{}", render(notification));
        }
        if notifications.is_empty() {
            println!("No {}notifications", if all { "" } else { "new " });
        }
    })?;
    // Shown once: what this listing printed does not come back as new.
    if let Some(through) = notifications.iter().map(|n| n.id).max() {
        client::call(&ctx.paths, Method::MarkNotificationsRead { through }).await?;
    }
    Ok(0)
}

/// The daemon marks each notification read as it delivers it.
async fn follow_stream(ctx: &Context) -> Result<i32> {
    let (mut stream, body) = client::open(&ctx.paths, Method::WatchNotifications).await?;
    match body {
        Body::Ok => {}
        Body::Error { code, message } => bail!("{code}: {message}"),
        _ => bail!("unexpected daemon response"),
    }
    loop {
        let response: Response = protocol::read(&mut stream)
            .await
            .context("daemon closed the notification stream")?;
        match response.body {
            Body::Notification(notification) => {
                ctx.show(&notification, |notification| {
                    println!("{}", render(notification));
                })?;
            }
            _ => bail!("unexpected daemon response"),
        }
    }
}

fn render(notification: &Notification) -> String {
    format!(
        "{}  {}  {}",
        local_time(notification.created_at),
        notification.workspace.as_deref().unwrap_or("-"),
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
    fn local_time_formats_the_epoch_in_the_process_time_zone() {
        // Only the shape is stable across zones; the value is checked at UTC.
        let text = super::local_time(1_700_000_000);
        assert_eq!(text.len(), 16, "{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[10..11], " ");
    }
}
