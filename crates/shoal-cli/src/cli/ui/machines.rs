//! The joined machine a command acts on, picked among the hub's other
//! machines: those online to pick, by name, and the offline ones listed
//! above them with when the hub last saw them, which cannot be picked.
use anyhow::{Result, ensure};
use shoal_protocol::hub::{Member, MemberKind, directory::HubView};

use super::{Entries, pick_with_header};
use crate::{cli::context::Context, time::UnixTime};

/// The other machine of `view` the user picks, by the name `--machine`
/// takes for it: its name, or its ID where another machine has that name.
pub fn pick_machine(ctx: &Context, prompt: &str, view: &HubView) -> Result<String> {
    let (online, offline) = other_machines(view);
    let header = offline_header(&offline, UnixTime::now());
    ensure!(
        !online.is_empty(),
        "no other joined machine is online{}",
        header
            .map(|offline| format!("; {offline}"))
            .unwrap_or_default()
    );
    let entries: Entries = (online.iter())
        .map(|machine| (machine.id.to_string(), machine.name.clone()))
        .collect();
    let id = pick_with_header(ctx, prompt, entries, header.as_deref())?;
    Ok(named(view, &id))
}

/// Whether `view` has another machine online to pick.
pub fn can_pick_machine(view: &HubView) -> bool {
    !other_machines(view).0.is_empty()
}

/// The machines of `view` but this one, online and offline, each by name.
fn other_machines(view: &HubView) -> (Vec<&Member>, Vec<&Member>) {
    let mut others: Vec<&Member> = (view.directory.machines.iter())
        .filter(|member| member.kind == MemberKind::Machine && member.id != view.machine)
        .collect();
    others.sort_by(|a, b| a.name.cmp(&b.name));
    others.into_iter().partition(|machine| machine.connected)
}

/// The line that lists the `offline` machines with when the hub last saw
/// each, as of `now`; none while every machine is online.
fn offline_header(offline: &[&Member], now: UnixTime) -> Option<String> {
    if offline.is_empty() {
        return None;
    }
    let listed: Vec<String> = (offline.iter())
        .map(|machine| match machine.last_seen {
            Some(seen) => format!("{} (seen {})", machine.name, seen.local_ago(now)),
            None => format!("{} (never seen)", machine.name),
        })
        .collect();
    Some(format!("Offline: {}", listed.join(", ")))
}

/// How `--machine` names the machine `id` of `view`: by its name, unless
/// another machine has that name too.
fn named(view: &HubView, id: &str) -> String {
    let machines = &view.directory.machines;
    let Some(machine) = machines.iter().find(|machine| machine.id.as_str() == id) else {
        return id.to_owned();
    };
    let shared = (machines.iter())
        .filter(|other| other.name == machine.name)
        .count();
    match shared {
        1 => machine.name.clone(),
        _ => id.to_owned(),
    }
}

#[cfg(test)]
mod tests;
