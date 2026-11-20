//! What `shoal move` asks before the daemon ends spaces: the spaces that
//! end with those named and the holds the move releases, and the refusals
//! of a move to standard output that would end this command first.
use anyhow::{Result, ensure};
use shoal_protocol::model::ByKind;

use crate::{
    cli::{context::Context, ui},
    model::{Space, SpaceHold},
    transfer::{MoveDestination, MoveOrder},
};
use shoal_ids::space_ids::SpaceId;
use shoal_workers::moves::{
    Planned,
    checks::{Checked, Ending},
};

/// Ask, unless `yes` confirms it, before the move `order` that `planned`
/// describes ends spaces where that ends more than the spaces named or
/// releases holds.
pub(super) fn confirm(
    ctx: &Context,
    yes: bool,
    order: &MoveOrder,
    planned: &[Planned],
) -> Result<()> {
    let checks: Vec<&Checked> = (planned.iter())
        .filter_map(|planned| match planned {
            Planned::Ending { checked, .. } => Some(checked),
            Planned::Copied { .. } | Planned::Removed { .. } => None,
        })
        .collect();
    let Some(action) = ending_action(heading(order, &checks), &checks).filter(|_| !yes) else {
        return Ok(());
    };
    ensure!(ui::confirm(ctx, &action, "--yes")?, "move canceled");
    Ok(())
}

/// What the confirmation of `order`, ending the spaces `checks` describe,
/// asks under.
fn heading(order: &MoveOrder, checks: &[&Checked]) -> String {
    let to = match &order.destination {
        MoveDestination::Ssh(destination) => format!(" to {}", destination.host),
        MoveDestination::File { path } | MoveDestination::Directory { path } => {
            format!(" to {}", path.display())
        }
        MoveDestination::Client { .. } => String::new(),
    };
    match checks {
        [check] => format!("Move {}{to}", label(check)),
        _ => format!("Move {} spaces{to}", checks.len()),
    }
}

/// The space with its kind, as a confirmation names it.
fn label(check: &Checked) -> String {
    match &check.space {
        Ending::Workspace(check) => format!("workspace {}", check.space.name),
        Ending::Chat(chat) => format!("chat {}", chat.name),
    }
}

/// The question [`confirm`] asks under `heading`; none when ending the
/// spaces `checks` describe ends only them and releases no holds.
fn ending_action(heading: String, checks: &[&Checked]) -> Option<String> {
    let mut carried: Vec<&Space> = Vec::new();
    for space in checks.iter().flat_map(|check| &check.carried) {
        if !carried.iter().any(|listed| listed.id == space.id) {
            carried.push(space);
        }
    }
    let own = (checks.iter()).map(|check| (check.id(), check.name().as_str(), check.own_holds()));
    let below = (carried.iter()).map(|space| (&space.id, space.name.as_str(), &space.holds[..]));
    let mut held: Vec<(&SpaceId, &str, &[SpaceHold])> = Vec::new();
    for space in own.chain(below) {
        if !space.2.is_empty() && !held.iter().any(|listed| listed.0 == space.0) {
            held.push(space);
        }
    }
    if carried.is_empty() && held.is_empty() {
        return None;
    }
    let mut action = heading;
    if !carried.is_empty() {
        action.push_str("\nEnds these spaces below here too, stopping their agents:");
        for space in &carried {
            action.push_str(&format!("\n  {} ({})", space.name, described(space)));
        }
    }
    if !held.is_empty() {
        action.push_str("\nRemoves the spaces here and releases these holds:");
    }
    for (_, name, holds) in &held {
        for hold in *holds {
            action.push_str("\n  ");
            if checks.len() > 1 || held.len() > 1 {
                action.push_str(&format!("{name}: "));
            }
            action.push_str(&hold.name);
            if let Some(reason) = &hold.reason {
                action.push_str(&format!(" ({reason})"));
            }
        }
    }
    Some(action)
}

/// The kind of `space` with its agent, as the confirmation lists it: a
/// chat's agent by name, a workspace's while it runs.
fn described(space: &Space) -> String {
    let agent = match space.clone().by_kind() {
        ByKind::Chat(chat) => chat.agent.as_ref().map(ToString::to_string),
        ByKind::Workspace(_) => space.running.then(|| "agent running".to_owned()),
    };
    match agent {
        Some(agent) => format!("{}, {agent}", space.kind()),
        None => space.kind().to_string(),
    }
}

/// Refuse a move to standard output that ends this command before it
/// copied the package: one that ends the chat this process runs in, or a
/// workspace this terminal closes with.
pub(super) async fn ensure_stream_survives(ctx: &Context, planned: &[Planned]) -> Result<()> {
    for planned in planned {
        let Planned::Ending { checked, .. } = planned else {
            continue;
        };
        let spaces = std::iter::once((checked.id(), checked.name().as_str())).chain(
            checked
                .carried
                .iter()
                .map(|space| (&space.id, space.name.as_str())),
        );
        for (id, name) in spaces {
            ensure!(
                crate::env::own_space().as_ref() != Some(id),
                "this process runs in {name}, which the move ends before the package reaches standard output; move it to a file with -o, or from another terminal"
            );
            ensure!(
                !crate::cli::terminal::closes_with(&ctx.paths, id).await?,
                "this terminal closes when the move ends {name}, ending the stream; move it to a file with -o, or from another terminal"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
