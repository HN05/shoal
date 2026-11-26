//! `shoal repo rm`, here or on other joined machines.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_client::output::repositories::{RemovedRepository, RepositoryElsewhere};

use super::elsewhere;
use crate::{
    cli::{context::Context, ui},
    model::Repository,
    refusal::Reason,
    repository::{self, Selector},
};

mod confirmation;
mod targets;

pub(super) use targets::Machines;
use targets::{Registered, Resolved, Target};

pub(super) async fn remove(
    ctx: &Context,
    repository: Option<String>,
    machines: Machines,
    confirmed: bool,
) -> Result<i32> {
    let targets = targets(ctx, repository, machines).await?;
    confirm(ctx, &targets, confirmed).await?;
    let removed = remove_each(ctx, targets).await?;
    ctx.show(&removed, |_| {})?;
    Ok(0)
}

async fn targets(ctx: &Context, named: Option<String>, machines: Machines) -> Result<Vec<Target>> {
    let here = crate::client::repositories::list(&ctx.paths).await?;
    let Named {
        named,
        local,
        not_here,
        machines,
    } = match named {
        Some(named) => named_here(&here, named, machines).await?,
        None => match chosen(ctx, &here).await? {
            Chosen::Named(named) => named_here(&here, named, machines).await?,
            Chosen::Elsewhere(other) => picked(&here, other, machines).await,
        },
    };
    let machines = &machines;
    // A repository registered here is removed here, without asking the hub.
    if let (Some(local), Machines::Found) = (local, machines) {
        return Ok(vec![Target::Here(local.clone())]);
    }
    // Where it names machines, a removal that could not ask the hub would
    // act on fewer than it names.
    let (elsewhere, view) = match machines {
        Machines::Found => elsewhere::registered_or_left_out(ctx).await,
        Machines::Named(_) | Machines::All => elsewhere::registered(ctx).await?,
    };
    let registered = Registered {
        here: local,
        here_origin: match local {
            Some(local) => identity(local).await,
            None => None,
        },
        elsewhere: &elsewhere,
        view: view.as_ref(),
    };
    match targets::resolve(&named, &registered, machines)? {
        Resolved::Targets(targets) if targets.is_empty() => {
            Err(not_here.unwrap_or_else(|| anyhow::anyhow!("no machine registers {named}")))
        }
        Resolved::Targets(targets) => Ok(targets),
        Resolved::Choose(candidates) => Ok(vec![Target::Elsewhere(pick(ctx, &named, candidates)?)]),
    }
}

enum Chosen {
    Named(String),
    Elsewhere(RepositoryElsewhere),
}

async fn chosen(ctx: &Context, here: &[Repository]) -> Result<Chosen> {
    if let Some(id) = super::super::issues::current_repository_id(ctx, here).await? {
        return Ok(Chosen::Named(id.to_string()));
    }
    ensure!(
        ctx.interactive(),
        "no current registered repository; {}",
        super::REPOSITORY
    );
    let (elsewhere, _) = elsewhere::registered_or_left_out(ctx).await;
    let mut entries = ui::repository_choices(here.to_vec()).await;
    entries.extend(elsewhere.iter().map(|other| (key(other), label(other))));
    let picked = ui::pick(ctx, "Repository> ", entries)?;
    Ok(
        match elsewhere.into_iter().find(|other| key(other) == picked) {
            Some(other) => Chosen::Elsewhere(other),
            None => Chosen::Named(picked),
        },
    )
}

fn pick(
    ctx: &Context,
    named: &str,
    candidates: Vec<RepositoryElsewhere>,
) -> Result<RepositoryElsewhere> {
    if !ctx.interactive() {
        let machines: Vec<&str> = candidates
            .iter()
            .map(|other| other.machine.as_str())
            .collect();
        bail!(
            "repository {named} is registered on machines {}; pass --machine NAME, repeatable, or --all",
            machines.join(", ")
        );
    }
    let entries = candidates
        .iter()
        .map(|other| (key(other), label(other)))
        .collect();
    let picked = ui::pick(ctx, "Machine> ", entries)?;
    (candidates.into_iter())
        .find(|other| key(other) == picked)
        .context("the picked repository is gone")
}

/// How a picker tells another machine's registration from every other.
fn key(other: &RepositoryElsewhere) -> String {
    format!("{}/{}", other.machine_id, other.registered.id)
}

fn label(other: &RepositoryElsewhere) -> String {
    format!("{}  {}  on {}", other.name(), other.source(), other.machine)
}

struct Named<'a> {
    named: String,
    local: Option<&'a Repository>,
    /// Why this machine has none, which stands when no machine has it.
    not_here: Option<anyhow::Error>,
    machines: Machines,
}

async fn named_here(here: &[Repository], named: String, machines: Machines) -> Result<Named<'_>> {
    let selector = ui::repository_selector(named.clone())?;
    match repository::select(here, selector).await {
        Ok(local) => Ok(Named {
            named,
            local: Some(local),
            not_here: None,
            machines,
        }),
        Err(error) if Reason::of(&error) == Some(Reason::NotFound) => Ok(Named {
            named,
            local: None,
            not_here: Some(error),
            machines,
        }),
        Err(error) => Err(error),
    }
}

/// `other`, another machine's registration the user picked, named by its
/// ID there and here by its origin, which the `machines` a removal names
/// match it by; with none named, it acts on that machine alone.
async fn picked(here: &[Repository], other: RepositoryElsewhere, machines: Machines) -> Named<'_> {
    let origin = other.registered.repository.origin.as_deref();
    let local = of_origin(here, origin).await;
    let machines = match machines {
        Machines::Found => Machines::Named(vec![other.machine_id.clone()]),
        named => named,
    };
    Named {
        named: other.registered.id.to_string(),
        local,
        not_here: None,
        machines,
    }
}

async fn of_origin<'a>(here: &'a [Repository], origin: Option<&str>) -> Option<&'a Repository> {
    let origin = origin?;
    for local in here {
        if identity(local).await.as_deref() == Some(origin) {
            return Some(local);
        }
    }
    None
}

/// The key of the origin of `local`, as the hub's reports give the other
/// machines' registrations of it; none for a checkout without one.
async fn identity(local: &Repository) -> Option<String> {
    (crate::repository::identity(&local.source).await)
        .ok()
        .flatten()
}

/// Refused without asking where nobody can answer.
async fn confirm(ctx: &Context, targets: &[Target], confirmed: bool) -> Result<()> {
    if confirmed {
        return Ok(());
    }
    let action = match ctx.interactive() {
        true => confirmation::describe(ctx, targets).await?,
        false => confirmation::briefly(targets),
    };
    ensure!(
        ui::confirm(ctx, &action, "--yes")?,
        "repository removal canceled"
    );
    Ok(())
}

/// This machine's removal goes last, so another machine that refuses its
/// removal stops it before anything here goes.
async fn remove_each(ctx: &Context, mut targets: Vec<Target>) -> Result<Vec<RemovedRepository>> {
    targets.sort_by_key(|target| matches!(target, Target::Here(_)));
    let mut removed: Vec<RemovedRepository> = Vec::new();
    for target in targets {
        match remove_one(ctx, &target).await {
            Ok(one) => removed.push(one),
            Err(error) if removed.is_empty() => return Err(error),
            Err(error) => {
                let done: Vec<String> = removed.iter().map(where_removed).collect();
                return Err(error.context(format!("removed it {} first", done.join(", "))));
            }
        }
    }
    Ok(removed)
}

async fn remove_one(ctx: &Context, target: &Target) -> Result<RemovedRepository> {
    let (selector, machine) = request(target);
    let shown = machine_name(target);
    let on = (shown.as_deref())
        .map(|machine| format!(" on {machine}"))
        .unwrap_or_default();
    let removal = crate::client::repositories::remove(&ctx.paths, selector, machine);
    let removal = (ctx.progress(&format!("Removing repository{on}"), removal)).await?;
    if !ctx.json {
        outln!(
            "Deleted {} and {} Shoal workspaces{on}",
            removal.path.display(),
            removal.workspaces_removed,
        );
    }
    Ok(RemovedRepository {
        machine: shown,
        removal,
    })
}

fn request(target: &Target) -> (Selector, Option<String>) {
    match target {
        Target::Here(local) => (local.id.clone().into(), None),
        Target::Elsewhere(other) => (
            other.registered.id.clone().into(),
            Some(other.machine_id.clone()),
        ),
    }
}

fn machine_name(target: &Target) -> Option<String> {
    match target {
        Target::Here(_) => None,
        Target::Elsewhere(other) => Some(other.machine.clone()),
    }
}

fn where_removed(removed: &RemovedRepository) -> String {
    match &removed.machine {
        Some(machine) => format!("on {machine}"),
        None => "here".to_owned(),
    }
}

#[cfg(test)]
mod tests;
