//! `shoal alert list` shows alerts: the caller's own, the user's from the
//! user's terminal or its own space's from an agent, a space's with
//! `--space`, or every recipient's it may read with `--all`; in a picker on
//! a terminal, else as a list. Showing one recipient's marks them read
//! unless `--keep-unread`; `--follow` keeps the terminal on the user's stream and
//! raises each one as a terminal notification. `withdraw` takes back an
//! unread alert the caller sent, and `clear` deletes the user's.
use std::io::{IsTerminal, Write};

use anyhow::{Result, ensure};

use crate::{
    cli::{
        args::coordination::AlertListArgs,
        client,
        context::Context,
        installed::arguments_with,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    client::alerts::{self, Watch},
    installed::Executable,
};
use shoal_protocol::{
    alerts::{Alert, Sender},
    protocol::AlertsFor,
    space_names::SpaceSelector,
};

pub(super) mod delivery;
mod menu;

/// Whose alerts a listing shows, and whether showing them marks them read,
/// as it does those of one recipient.
struct Listing {
    to: AlertsFor,
    marks_read: bool,
}

/// Show the alerts `args` ask for: follow the user's, pick one on a
/// terminal, or print them.
pub(super) async fn list(ctx: &Context, args: AlertListArgs) -> Result<i32> {
    if args.follow {
        return follow(ctx, args.keep_unread, args.resume.unwrap_or(0)).await;
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
        let ids = shown.iter().map(|alert| alert.id).collect();
        alerts::mark_read(&ctx.paths, ids).await?;
    }
    if !picked && args.unread && matches!(listing.to, AlertsFor::User) {
        remaining(ctx, &shown, args.keep_unread).await?;
    }
    Ok(code)
}

/// Whose alerts `args` asks for: every recipient's, the space's it names,
/// or else the caller's own.
async fn listing(ctx: &Context, args: &AlertListArgs) -> Result<Listing> {
    if args.all {
        return Ok(Listing {
            to: AlertsFor::Every,
            marks_read: false,
        });
    }
    if let Some(space) = &args.space {
        let space = ui::select_space(ctx, Some(space.clone()), Fallback::CurrentDirectory).await?;
        return Ok(Listing {
            to: AlertsFor::Space { space },
            marks_read: true,
        });
    }
    let to = match in_own_space() {
        true => AlertsFor::Space {
            space: delivery::own_space(ctx).await?,
        },
        false => {
            refuse_workspace_processes(ctx)?;
            AlertsFor::User
        }
    };
    Ok(Listing {
        to,
        marks_read: true,
    })
}

/// Whether this process acts in a space of its own, a scoped one or its
/// chat's agent, whose alerts are its own and whose sends go up.
pub(super) fn in_own_space() -> bool {
    crate::env::is_scoped() || crate::env::own_chat().is_some()
}

/// The space this process sends and withdraws alerts as, which the daemon
/// takes from a scoped caller's scope; none for the user.
pub(super) async fn sending_space(ctx: &Context) -> Result<Option<SpaceSelector>> {
    match in_own_space() {
        true => Ok(Some(delivery::own_space(ctx).await?)),
        false => Ok(None),
    }
}

/// Print `shown`, or that there are none.
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

/// The daemon refuses a scoped process the user's alerts unless its
/// `notifications` permission allows them; this refuses a descendant that
/// cleared its scope token, which the daemon would take for the user.
fn refuse_workspace_processes(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "workspace processes read and clear the user's alerts only with their scope token"
    );
    Ok(())
}

pub(super) async fn withdraw(ctx: &Context, id: i64) -> Result<i32> {
    let from = sending_space(ctx).await?;
    let withdrawn = alerts::withdraw(&ctx.paths, id, from).await?;
    ctx.emit(
        &format!("Withdrew alert {id}: {}", withdrawn.message),
        &withdrawn,
    )?;
    Ok(0)
}

/// The daemon marks each alert read as it delivers it unless `keep_unread`;
/// the stream resumes after the last one printed. On a terminal, each one
/// is also raised as a terminal notification so the emulator can show it
/// while another window has focus.
pub(super) async fn follow(ctx: &Context, keep_unread: bool, after: i64) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let executable = Executable::current();
    let mut watch = Watch::start(&ctx.paths, keep_unread, after).await?;
    let palette = Palette::stdout(ctx.json);
    let raise = !ctx.json && std::io::stdout().is_terminal();
    loop {
        let alert = watch.next(&ctx.paths).await.map_err(|error| {
            let delivered = watch.delivered();
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

/// The alert's row `line`, then a line for each file sent with it.
fn with_files(line: &str, alert: &Alert) -> String {
    std::iter::once(line.to_owned())
        .chain((alert.files.iter()).map(|file| format!("  Attached file: {}", file.path.display())))
        .collect::<Vec<_>>()
        .join("\n")
}

/// When, read at `now`, the alert's number, who sent it, who it is for,
/// and its text.
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
/// another machine by its ID.
fn sender(alert: &Alert) -> String {
    match (alert.sender, alert.space_label(), &alert.space_id) {
        (Sender::User, ..) => "user".to_owned(),
        (_, Some(space), _) => space,
        (Sender::Space, None, Some(id)) => id.to_string(),
        (_, None, _) => "shoal".to_owned(),
    }
}

/// The alert's text, after the space that sent it or that it is about,
/// followed by a line for each file attached to it.
pub(super) fn shown(alert: &Alert) -> String {
    let attached =
        (alert.attachments().into_iter()).map(|path| format!("Attached file: {}", path.display()));
    std::iter::once(alert.text())
        .filter(|text| !text.is_empty())
        .chain(attached)
        .collect::<Vec<_>>()
        .join("\n")
}

/// `text` with its later lines indented by `indent`, so a multi-line alert
/// reads as one entry of a list.
pub(super) fn hanging_indent(text: &str, indent: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(n, line)| {
            if n == 0 || line.is_empty() {
                line.to_owned()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests;
