//! What a move asks its caller before it ends more than it names.
use shoal_protocol::model::ByKind;

use super::{
    Planned,
    checks::{Checked, Ending},
};
use crate::model::{Space, SpaceHold};
use shoal_core::transfer::{MoveDestination, MoveOrder};
use shoal_ids::space_ids::SpaceId;

/// What the move `order` that `planned` describes asks before it ends
/// anything; none when it ends only the spaces it names and releases no
/// holds.
pub(super) fn question(order: &MoveOrder, planned: &[Planned]) -> Option<String> {
    let checks: Vec<&Checked> = (planned.iter())
        .filter_map(|planned| match planned {
            Planned::Ending { checked, .. } => Some(checked),
            Planned::Copied { .. } | Planned::Removed { .. } => None,
        })
        .collect();
    ending_action(heading(order, &checks), &checks)
}

/// What the confirmation of `order`, ending the spaces `checks` describe,
/// asks under.
fn heading(order: &MoveOrder, checks: &[&Checked]) -> String {
    let to = match (
        &order.destination,
        super::packages::other_machine(&order.destination),
    ) {
        (_, Some(machine)) => format!(" to {machine}"),
        (MoveDestination::File { path } | MoveDestination::Directory { path }, None) => {
            format!(" to {}", path.display())
        }
        _ => String::new(),
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

/// The question [`question`] asks under `heading`; none when ending the
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

#[cfg(test)]
mod tests;
