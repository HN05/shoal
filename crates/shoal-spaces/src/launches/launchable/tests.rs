use shoal_protocol::hub::Member;
use shoal_schedule::placement::snapshot::{Capacity, Registered};

use super::{listed, refusal};

const ORIGIN: &str = "example.com/owner/app";

/// The connected machine `name`, which launches `agents` in the repository.
fn machine(name: &str, agents: &[&str]) -> Member {
    let mut member = Member::unlisted(name.into());
    member.connected = true;
    member.capacity = Some(Capacity {
        repositories: vec![Registered {
            origin: ORIGIN.into(),
            agents: agents.iter().map(|agent| (*agent).into()).collect(),
        }],
        ..Capacity::default()
    });
    member
}

#[test]
fn a_start_is_refused_where_the_hub_lists_the_machine_without_its_agent() {
    let (laptop, desk) = (
        machine("laptop", &["claude"]),
        machine("desk", &["claude", "codex"]),
    );
    let mut offline = machine("server", &["codex"]);
    offline.connected = false;
    let lists = |member: &Member, agent: &str| {
        listed(member, Some(ORIGIN)).map(|agents| agents.iter().any(|a| a == agent))
    };
    assert_eq!(lists(&laptop, "codex"), Some(false));
    assert_eq!(lists(&desk, "codex"), Some(true));
    // It names the connected machines that list the agent.
    let machines = [laptop.clone(), desk, offline];
    let refused = refusal(&laptop, &machines, Some(ORIGIN), "codex");
    assert!(refused.contains("machines that can: desk"), "{refused}");
    let refused = refusal(&laptop, &machines, Some(ORIGIN), "pi");
    assert!(refused.contains("no connected machine can"), "{refused}");
}

#[test]
fn a_machine_the_hub_lists_nothing_for_decides_on_its_own() {
    let mut undescribed = machine("desk", &[]);
    undescribed.capacity = None;
    assert_eq!(listed(&undescribed, Some(ORIGIN)), None);
    // Nor for a repository it has not registered.
    assert_eq!(
        listed(&machine("desk", &[]), Some("example.com/owner/other")),
        None
    );
}
