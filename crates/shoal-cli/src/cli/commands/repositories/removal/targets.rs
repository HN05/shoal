//! Where `shoal repo rm` removes a repository. Each other machine is matched by
//! its own registration of the name or of the same origin.
use anyhow::{Result, bail};
use shoal_client::output::repositories::RepositoryElsewhere;
use shoal_protocol::{
    hub::{Member, directory::HubView},
    model::Repository,
    repository::url_key,
};

use crate::time::UnixTime;

pub(in crate::cli::commands::repositories) enum Machines {
    /// Wherever the repository is registered: here, else the one other
    /// machine that registers it.
    Found,
    Named(Vec<String>),
    All,
}

impl Machines {
    pub fn of(named: Vec<String>, all: bool) -> Self {
        match (all, named.is_empty()) {
            (true, _) => Self::All,
            (false, true) => Self::Found,
            (false, false) => Self::Named(named),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum Target {
    Here(Repository),
    Elsewhere(RepositoryElsewhere),
}

pub(super) struct Registered<'a> {
    pub here: Option<&'a Repository>,
    /// The key of that repository's origin.
    pub here_origin: Option<String>,
    pub elsewhere: &'a [RepositoryElsewhere],
    /// The hub's view of its machines; none on a machine in no hub.
    pub view: Option<&'a HubView>,
}

#[derive(Debug)]
pub(super) enum Resolved {
    /// These targets; none where no machine registers the repository.
    Targets(Vec<Target>),
    /// Not registered here but on each of these other machines, of which
    /// the caller picks one.
    Choose(Vec<RepositoryElsewhere>),
}

/// Refused while a machine it would act on is offline.
pub(super) fn resolve(
    named: &str,
    registered: &Registered,
    machines: &Machines,
) -> Result<Resolved> {
    let selected: Vec<&RepositoryElsewhere> = (registered.elsewhere.iter())
        .filter(|other| names(other, named))
        .collect();
    let origins: Vec<&str> = (registered.here_origin.as_deref().into_iter())
        .chain(selected.iter().filter_map(|other| origin(other)))
        .collect();
    let targets = match machines {
        Machines::Found => match found(registered.here, &selected)? {
            Resolved::Targets(targets) => targets,
            choose => return Ok(choose),
        },
        Machines::Named(machines) => {
            let mut targets = Vec::new();
            for machine in machines {
                let target = on_machine(machine, named, registered, &origins)?;
                if !targets.iter().any(|other| same_machine(other, &target)) {
                    targets.push(target);
                }
            }
            targets
        }
        Machines::All => everywhere(named, registered, &origins)?,
    };
    ensure_online(&targets, registered.view)?;
    Ok(Resolved::Targets(targets))
}

/// Here, where `here` is registered, else the one other machine of the
/// `selected` registrations, or a choice among several.
fn found(here: Option<&Repository>, selected: &[&RepositoryElsewhere]) -> Result<Resolved> {
    if let Some(here) = here {
        return Ok(Resolved::Targets(vec![Target::Here(here.clone())]));
    }
    let mut machines: Vec<&str> = selected
        .iter()
        .map(|other| other.machine_id.as_str())
        .collect();
    machines.dedup();
    Ok(match machines.as_slice() {
        [] => Resolved::Targets(Vec::new()),
        [_] => Resolved::Targets(vec![Target::Elsewhere(one(
            selected,
            &selected[0].machine,
        )?)]),
        _ => Resolved::Choose(selected.iter().map(|&other| other.clone()).collect()),
    })
}

/// The repository `named` names on the joined machine `machine` names:
/// its registration of that name, else of one of `origins`.
fn on_machine(
    machine: &str,
    named: &str,
    registered: &Registered,
    origins: &[&str],
) -> Result<Target> {
    let Some(view) = registered.view else {
        bail!("this machine joined no hub, so its repositories are here; leave out --machine");
    };
    let member = view
        .directory
        .named_machine(machine)
        .map_err(anyhow::Error::msg)?;
    if member.id == view.machine {
        return match registered.here {
            Some(here) => Ok(Target::Here(here.clone())),
            None => bail!("this machine registers no repository {named}; shoal repo ls lists them"),
        };
    }
    match of_machine(member, named, registered.elsewhere, origins)? {
        Some(other) => Ok(Target::Elsewhere(other)),
        None => bail!(
            "machine {} registers no repository {named}; shoal repo ls lists them",
            member.name
        ),
    }
}

fn everywhere(named: &str, registered: &Registered, origins: &[&str]) -> Result<Vec<Target>> {
    let mut targets: Vec<Target> = registered
        .here
        .cloned()
        .map(Target::Here)
        .into_iter()
        .collect();
    let Some(view) = registered.view else {
        return Ok(targets);
    };
    for member in &view.directory.machines {
        if member.id == view.machine {
            continue;
        }
        if let Some(other) = of_machine(member, named, registered.elsewhere, origins)? {
            targets.push(Target::Elsewhere(other));
        }
    }
    Ok(targets)
}

/// The registration of `member` among `elsewhere` that `named` names, or
/// else that has one of `origins`; none where it has neither.
fn of_machine(
    member: &Member,
    named: &str,
    elsewhere: &[RepositoryElsewhere],
    origins: &[&str],
) -> Result<Option<RepositoryElsewhere>> {
    let its: Vec<&RepositoryElsewhere> = (elsewhere.iter())
        .filter(|other| other.machine_id == member.id.as_str())
        .collect();
    let by_name: Vec<&RepositoryElsewhere> = its
        .iter()
        .copied()
        .filter(|other| names(other, named))
        .collect();
    let chosen = match by_name.is_empty() {
        false => by_name,
        true => (its.into_iter())
            .filter(|other| origin(other).is_some_and(|origin| origins.contains(&origin)))
            .collect(),
    };
    match chosen.is_empty() {
        true => Ok(None),
        false => one(&chosen, &member.name).map(Some),
    }
}

fn one(candidates: &[&RepositoryElsewhere], machine: &str) -> Result<RepositoryElsewhere> {
    match candidates {
        [one] => Ok((*one).clone()),
        _ => bail!(
            "machine {machine} registers several repositories it could be: {}; name one by the ID shoal repo ls --json shows",
            (candidates.iter())
                .map(|other| format!("{} ({})", other.name(), other.registered.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Whether `named` names `other`: by its ID, its name or its origin.
fn names(other: &RepositoryElsewhere, named: &str) -> bool {
    other.registered.id.as_str() == named
        || other.name() == named
        || origin(other).is_some_and(|origin| origin == url_key(named))
}

fn origin(other: &RepositoryElsewhere) -> Option<&str> {
    other.registered.repository.origin.as_deref()
}

fn same_machine(a: &Target, b: &Target) -> bool {
    match (a, b) {
        (Target::Here(_), Target::Here(_)) => true,
        (Target::Elsewhere(a), Target::Elsewhere(b)) => a.machine_id == b.machine_id,
        _ => false,
    }
}

fn ensure_online(targets: &[Target], view: Option<&HubView>) -> Result<()> {
    let Some(view) = view else {
        return Ok(());
    };
    let now = UnixTime::now();
    let offline: Vec<String> = (targets.iter())
        .filter_map(|target| match target {
            Target::Elsewhere(other) => (view.directory.machines.iter())
                .find(|member| member.id.as_str() == other.machine_id && !member.connected),
            Target::Here(_) => None,
        })
        .map(|member| member.offline(now))
        .collect();
    if !offline.is_empty() {
        bail!(
            "{}; a repository is removed only on machines that are online, so nothing was removed",
            offline.join("; ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
