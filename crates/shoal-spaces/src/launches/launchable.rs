//! Refusing a start on another machine that the hub lists as unable to
//! launch its agent, before anything goes there.
use anyhow::{Result, bail};
use shoal_protocol::hub::Member;
use shoal_protocol::launches::Launch;

use crate::Spaces;
use crate::daemon::Daemon;

impl Spaces {
    /// Refused when the hub lists `machine` without the agent `launch`
    /// names, in the repository of the origin URL `origin`, or outside a
    /// repository without one. The refusal names the connected machines that
    /// list it. A launch that leaves the agent to that machine's default, a
    /// machine that has not described itself and one that has not
    /// registered the repository go ahead, for that machine to decide.
    pub(super) async fn ensure_launches(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        machine: &Member,
        origin: Option<&str>,
    ) -> Result<()> {
        let Some(agent) = &launch.agent else {
            return Ok(());
        };
        let origin = origin.map(shoal_protocol::repository::url_key);
        let (origin, agent) = (origin.as_deref(), agent.name());
        if listed(machine, origin).is_none_or(|agents| agents.iter().any(|name| name == agent)) {
            return Ok(());
        }
        let machines = daemon.joined_machines().await?;
        bail!(refusal(machine, &machines, origin, agent))
    }
}

/// The agents the hub lists `machine` as launching in the repository of
/// `origin`, or outside one; none where it lists none.
fn listed<'a>(machine: &'a Member, origin: Option<&str>) -> Option<&'a [String]> {
    (machine.capacity.as_ref()).and_then(|capacity| capacity.agents_in(origin))
}

/// Why a start of `agent` in the repository of `origin` does not go to
/// `machine`, naming the other connected `machines` that list it.
fn refusal(machine: &Member, machines: &[Member], origin: Option<&str>, agent: &str) -> String {
    let launching: Vec<&str> = (machines.iter())
        .filter(|other| other.connected && other.id != machine.id)
        .filter(|other| {
            listed(other, origin).is_some_and(|agents| agents.iter().any(|a| a == agent))
        })
        .map(|other| other.name.as_str())
        .collect();
    let place = match origin {
        Some(_) => "in this repository",
        None => "outside a repository",
    };
    let others = match launching.is_empty() {
        true => "no connected machine can".to_owned(),
        false => format!("machines that can: {}", launching.join(", ")),
    };
    format!(
        "machine {} cannot launch agent {agent} {place}; {others}",
        machine.name
    )
}

#[cfg(test)]
mod tests;
