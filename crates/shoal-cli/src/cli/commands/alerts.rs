//! `shoal alert`: listing, following, withdrawing and clearing alerts.
use std::io::{IsTerminal, Write};

use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::coordination::AlertListArgs,
        client,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::alerts::{self, Watch},
    installed::{Executable, arguments::arguments_with},
};
use shoal_protocol::{
    alerts::{Alert, AlertCursor, Sender},
    protocol::AlertsFor,
    space_names::SpaceSelector,
};

mod menu;

/// Whose alerts a listing shows, and whether showing them marks them read,
/// as it does those of one recipient.
struct Listing {
    to: AlertsFor,
    marks_read: bool,
}

pub(super) async fn list(ctx: &Context, args: AlertListArgs) -> Result<i32> {
    if args.follow {
        return follow(ctx, args.keep_unread, args.resume.unwrap_or_default()).await;
    }
    let listing = listing(ctx, &args).await?;
    let shown = alerts::list(&ctx.paths, listing.to.clone(), args.unread, args.limit).await?;
    let picked = !args.print && ctx.interactive_output();
    let code = match picked {
        true => menu::choose(ctx, &shown).await?,
        false => print(ctx, &shown, args.unread).map(|()| 0)?,
    };
    // Shown once: exactly what this listing showed does not come back as
    // new.
    if listing.marks_read && !args.keep_unread && !shown.is_empty() {
        alerts::mark_shown_read(&ctx.paths, &shown).await?;
    }
    if !picked && args.unread && matches!(listing.to, AlertsFor::User) {
        remaining(ctx, &shown, args.keep_unread).await?;
    }
    Ok(code)
}

async fn listing(ctx: &Context, args: &AlertListArgs) -> Result<Listing> {
    refuse_workspace_processes(ctx)?;
    if args.all {
        return Ok(Listing {
            to: AlertsFor::Every,
            marks_read: false,
        });
    }
    if let Some(space) = &args.space {
        let space =
            ui::select_space(ctx, Some(space.clone()), Fallback::CurrentDirectoryOnly).await?;
        return Ok(Listing {
            to: AlertsFor::Space { space },
            marks_read: true,
        });
    }
    let to = match in_own_space() {
        true => AlertsFor::Space {
            space: own_space(ctx).await?,
        },
        false => AlertsFor::User,
    };
    Ok(Listing {
        to,
        marks_read: true,
    })
}

/// Whether this process acts in a space of its own, a scoped one or its
/// chat's agent, whose alerts are its own and whose sends go up.
pub(in crate::cli::commands) fn in_own_space() -> bool {
    crate::env::is_scoped() || crate::env::own_chat().is_some()
}

/// The space of the caller's own agent, whose alerts it reads: its chat's
/// when it runs under one outside any workspace scope, otherwise its
/// workspace's.
pub(in crate::cli::commands) async fn own_space(ctx: &Context) -> Result<SpaceSelector> {
    ui::select_space(ctx, None, Fallback::OwnSpace).await
}

/// The space this process sends and withdraws alerts as, which the daemon
/// takes from a scoped caller's scope; none for the user, whom a process
/// that cleared its scope token may not act as.
pub(super) async fn sending_space(ctx: &Context) -> Result<Option<SpaceSelector>> {
    refuse_workspace_processes(ctx)?;
    match in_own_space() {
        true => Ok(Some(own_space(ctx).await?)),
        false => Ok(None),
    }
}

fn print(ctx: &Context, shown: &[Alert], unread: bool) -> Result<()> {
    ctx.show(&shown.to_vec(), |shown| {
        let now = crate::time::UnixTime::now();
        let rows = shown.iter().map(|alert| cells(alert, now)).collect();
        let lines = ui::aligned(rows, Palette::stdout(ctx.json));
        for (line, alert) in lines.iter().zip(shown) {
            outln!("{}", with_files(line, alert));
        }
        if shown.is_empty() {
            outln!("No {}alerts", if unread { "unread " } else { "" });
        }
    })
}

/// Say how many of the user's unread alerts a listing of the oldest left
/// for the next one.
async fn remaining(ctx: &Context, shown: &[Alert], keep_unread: bool) -> Result<()> {
    let Some(status) = client::status(&ctx.paths).await? else {
        return Ok(());
    };
    // A listing kept unread still counts what it printed.
    let printed = if keep_unread { shown.len() } else { 0 };
    let remaining = status.unread_notifications.saturating_sub(printed as u64);
    if remaining > 0 {
        eprintln!(
            "{remaining} more unread alert{}; run shoal alert list --unread again",
            if remaining == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

pub(super) async fn clear(ctx: &Context) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let cleared = alerts::clear(&ctx.paths).await?;
    ctx.show(&cleared, |cleared| match cleared.cleared {
        0 => outln!("No alerts"),
        n => outln!("Cleared {n} alert{}", if n == 1 { "" } else { "s" }),
    })?;
    Ok(0)
}

/// The daemon takes a scoped process's alerts only as its scope and
/// `notifications` permission allow; this refuses a descendant that cleared
/// its scope token, which the daemon would take for the user.
pub(super) fn refuse_workspace_processes(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "workspace processes read, send and withdraw alerts only with their scope token"
    );
    Ok(())
}

pub(super) async fn withdraw(ctx: &Context, id: i64, space: Option<SpaceSelector>) -> Result<i32> {
    let from = sending_space(ctx).await?;
    let withdrawn = alerts::withdraw(&ctx.paths, id, space, from).await?;
    ctx.emit(
        &format!("Withdrew alert {id}: {}", withdrawn.message),
        &withdrawn,
    )?;
    Ok(0)
}

/// The daemon marks each alert read as it delivers it unless `keep_unread`. On
/// a terminal, each alert is also raised as a terminal notification, so the
/// emulator can show it while another window has focus.
pub(super) async fn follow(ctx: &Context, keep_unread: bool, after: AlertCursor) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let executable = Executable::current();
    let mut watch = Watch::start(&ctx.paths, keep_unread, after).await?;
    let palette = Palette::stdout(ctx.json);
    let raise = !ctx.json && std::io::stdout().is_terminal();
    loop {
        let alert = watch.next(&ctx.paths).await.map_err(|error| {
            let delivered = watch.delivered().to_string();
            executable.resume_or(error, || arguments_with("--resume", Some(delivered)))
        })?;
        ctx.show(&alert, |alert| {
            outln!("{}", render(alert, palette, crate::time::UnixTime::now()));
        })?;
        if raise {
            crate::stdout::write(|stdout| {
                stdout.write_all(terminal_notification(&alert).as_bytes())?;
                stdout.flush()
            })?;
        }
    }
}

/// OSC 9 desktop notification, shown by iTerm2, Ghostty, WezTerm, and Kitty
/// and ignored by terminals without it. Control characters would end or
/// corrupt the sequence, so they are dropped from the text.
fn terminal_notification(alert: &Alert) -> String {
    let text: String = format!("Shoal {}: {}", sender(alert), alert.message)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    format!("\x1b]9;{text}\x07")
}

fn render(alert: &Alert, palette: Palette, now: crate::time::UnixTime) -> String {
    let line = ui::aligned(vec![cells(alert, now)], palette).concat();
    with_files(&line, alert)
}

fn with_files(line: &str, alert: &Alert) -> String {
    std::iter::once(line.to_owned())
        .chain((alert.files.iter()).map(|file| format!("  Attached file: {}", file.shown())))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cells(alert: &Alert, now: crate::time::UnixTime) -> [ui::Cell; 5] {
    let recipient = match &alert.recipient {
        Some(recipient) => format!("to {}", recipient.label()),
        None => "to user".to_owned(),
    };
    [
        (alert.created_at.local_ago(now), Some(Style::Muted)),
        (alert.id.to_string(), Some(Style::Muted)),
        (sender(alert), Some(Style::Heading)),
        (recipient, Some(Style::Muted)),
        (alert.message.clone(), None),
    ]
}

/// Who sent the alert, or for Shoal's the space it is about; a space of
/// another machine by its ID; then the other joined machine that keeps it.
fn sender(alert: &Alert) -> String {
    let sender = match (alert.sender, alert.space_label(), &alert.space_id) {
        (Sender::User, ..) => "user".to_owned(),
        (_, Some(space), _) => space,
        (Sender::Space, None, Some(id)) => id.to_string(),
        (_, None, _) => "shoal".to_owned(),
    };
    match &alert.machine {
        Some(machine) => format!("{sender} on {}", machine.name),
        None => sender,
    }
}

#[cfg(test)]
mod tests;
