//! `shoal hub join` and `shoal hub leave`.
use anyhow::{Context as _, Result};
use shoal_protocol::protocol::JoinVia;

use crate::cli::{args::hub::JoinArgs, context::Context, output::Style};

pub(super) async fn join(ctx: &Context, args: JoinArgs) -> Result<i32> {
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

fn command_words(command: &str) -> Result<Vec<String>> {
    let words = shlex::split(command).with_context(|| format!("unbalanced quotes in {command}"))?;
    anyhow::ensure!(!words.is_empty(), "--connect names no command");
    Ok(words)
}

pub(super) async fn leave(ctx: &Context) -> Result<i32> {
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
