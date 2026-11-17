//! `shoal hub`: pairing the user's machines and devices with the hub,
//! joining and leaving it, listing and revoking its members, and `shoal
//! daemon connect`, the relay a joined machine reaches the hub's daemon by.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::{HubRole, Listener, Member, Overview},
    protocol::JoinVia,
};

use crate::{
    cli::{
        args::hub::{HubCommand, JoinArgs},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    client::output::hub::Paired,
    time::UnixTime,
};

pub(in crate::cli::commands) async fn run(ctx: &Context, command: HubCommand) -> Result<i32> {
    match command {
        HubCommand::Pair => pair(ctx).await,
        HubCommand::Join(args) => join(ctx, args).await,
        HubCommand::Leave => leave(ctx).await,
        HubCommand::Ls => list(ctx).await,
        HubCommand::Revoke { member } => revoke(ctx, member).await,
    }
}

/// Print a new pairing code with its QR code.
async fn pair(ctx: &Context) -> Result<i32> {
    let pairing = crate::client::hub::pair(&ctx.paths).await?;
    let url = &pairing.code.url;
    let warnings = (url.unencrypted_remote())
        .then(|| {
            format!(
                "{url} is http:// to another machine, so links cross the network unencrypted; put a proxy or tunnel with TLS in front of the listener and set [machine.hub] url to its https:// address"
            )
        })
        .into_iter()
        .collect();
    let code = pairing.code.to_string();
    let qr = qrcode::QrCode::new(code.as_bytes()).context("encode the pairing code")?;
    let qr = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build();
    let paired = Paired { pairing, warnings };
    ctx.show(&paired, |paired| {
        let palette = Palette::stdout(false);
        for line in qr.lines() {
            outln!("{}", palette.paint(Style::Scannable, line));
        }
        outln!("{}", palette.paint(Style::Heading, &code));
        outln!(
            "Run shoal hub join with this code on the machine to join, by {}; it admits one join.",
            paired.pairing.expires_at.local_minutes()
        );
        for warning in &paired.warnings {
            eprintln!(
                "{} {warning}",
                Palette::stderr(false).paint(Style::Warning, "warning:")
            );
        }
    })?;
    Ok(0)
}

/// Join the hub, by a pairing code or through a command.
async fn join(ctx: &Context, args: JoinArgs) -> Result<i32> {
    let JoinArgs {
        code,
        connect,
        name,
    } = args;
    let via = match (code, connect) {
        (Some(code), _) => JoinVia::Code(code),
        (None, Some(command)) => JoinVia::Command(command_words(&command)?),
        (None, None) => unreachable!("clap requires a code or --connect"),
    };
    let membership = crate::client::hub::join(&ctx.paths, via, name).await?;
    let message = format!(
        "Joined the hub as {} ({})",
        membership.name, membership.member
    );
    ctx.emit_styled(Style::Success, &message, &membership)?;
    Ok(0)
}

/// The words of `command`, split as a shell would.
fn command_words(command: &str) -> Result<Vec<String>> {
    let words = shlex::split(command).with_context(|| format!("unbalanced quotes in {command}"))?;
    anyhow::ensure!(!words.is_empty(), "--connect names no command");
    Ok(words)
}

/// Leave the hub.
async fn leave(ctx: &Context) -> Result<i32> {
    let left = crate::client::hub::leave(&ctx.paths).await?;
    let message = match left.revoked {
        true => "Left the hub, which revoked this machine's credential".to_owned(),
        false => format!(
            "Left the hub, which could not be reached; run shoal hub revoke {} there",
            left.membership.member
        ),
    };
    ctx.emit_styled(Style::Success, &message, &left)?;
    Ok(0)
}

/// List the hub's listener and members.
async fn list(ctx: &Context) -> Result<i32> {
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
pub(in crate::cli::commands) fn listener_line(listener: &Listener) -> String {
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
async fn revoke(ctx: &Context, member: String) -> Result<i32> {
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
        HubRole::Hub { listener } => listener_line(listener),
        HubRole::Member {
            membership,
            route,
            connected,
            problem,
        } => {
            let state = match (connected, problem) {
                (true, _) => "connected".to_owned(),
                (false, Some(problem)) => format!("not connected: {problem}"),
                (false, None) => "not connected".to_owned(),
            };
            format!(
                "Member of the hub as {} ({}) through {route}, {state}",
                membership.name, membership.member
            )
        }
    }
}

/// Relay standard input and output to the daemon's socket.
pub(in crate::cli::commands) async fn relay(ctx: &Context) -> Result<i32> {
    let (input, output) = (tokio::io::stdin(), tokio::io::stdout());
    crate::client::hub::relay(&ctx.paths, input, output).await?;
    Ok(0)
}
