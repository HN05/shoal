//! `shoal hub ls` and `shoal hub revoke`: the hub's listener and members,
//! and what `shoal status` says of the hub role.
use anyhow::Result;
use shoal_protocol::hub::{HubRole, Listener, Member, Overview};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui,
    },
    time::UnixTime,
};

/// List the hub's listener and members.
pub(super) async fn list(ctx: &Context) -> Result<i32> {
    let overview = crate::client::hub::members(&ctx.paths).await?;
    ctx.show(&overview, |overview| {
        let palette = Palette::stdout(false);
        outln!("{}", listener_line(&overview.listener));
        for row in member_rows(overview, palette) {
            outln!("{row}");
        }
    })?;
    Ok(0)
}

/// What a line says of the hub's listener: where it listens, which Shoal
/// never encrypts, and the URL members reach it at.
fn listener_line(listener: &Listener) -> String {
    match (listener.address, &listener.error) {
        (Some(address), _) => format!(
            "Hub listening on {address}, unencrypted, at {}",
            listener.url
        ),
        (None, Some(error)) => format!("Hub not listening: {error}"),
        (None, None) => "Hub not listening".into(),
    }
}

fn member_rows(overview: &Overview, palette: Palette) -> Vec<String> {
    let now = UnixTime::now();
    let rows = (overview.members.iter())
        .map(|member| {
            let (state, style) = state(member, now);
            [
                (member.name.clone(), Some(Style::Heading)),
                (member.kind.to_string(), None),
                (
                    member.version.clone().unwrap_or_default(),
                    Some(Style::Muted),
                ),
                (state, Some(style)),
                (
                    (member.availability.as_ref())
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    None,
                ),
                (member.id.to_string(), Some(Style::Muted)),
            ]
        })
        .collect();
    ui::aligned(rows, palette)
}

/// Whether `member` is connected, the hub, or when it was last seen.
fn state(member: &Member, now: UnixTime) -> (String, Style) {
    match (member.hub, member.connected, member.last_seen) {
        (true, _, _) => ("hub".into(), Style::Success),
        (false, true, _) => ("connected".into(), Style::Success),
        (false, false, Some(seen)) => (format!("seen {}", seen.local_ago(now)), Style::Warning),
        (false, false, None) => ("never connected".into(), Style::Warning),
    }
}

/// Revoke a member's credential.
pub(super) async fn revoke(ctx: &Context, member: String) -> Result<i32> {
    let member = crate::client::hub::revoke(&ctx.paths, member).await?;
    let message = format!(
        "Revoked the credential of {} {} ({})",
        member.kind, member.name, member.id
    );
    ctx.emit_styled(Style::Success, &message, &member)?;
    Ok(0)
}

/// What `shoal status` says of the hub role the daemon takes.
pub(in crate::cli::commands) fn role_line(role: &HubRole) -> String {
    match role {
        HubRole::Hub {
            listener,
            availability,
        } => format!("{}; availability {availability}", listener_line(listener)),
        HubRole::Member {
            membership,
            route,
            connected,
            problem,
            availability,
        } => {
            let state = match (connected, problem) {
                (true, _) => "connected".to_owned(),
                (false, Some(problem)) => format!("not connected: {problem}"),
                (false, None) => "not connected".to_owned(),
            };
            let availability = (availability.as_ref())
                .map(|availability| format!("; availability {availability}"))
                .unwrap_or_default();
            format!(
                "Member of the hub as {} ({}) through {route}, {state}{availability}",
                membership.name, membership.member
            )
        }
    }
}
