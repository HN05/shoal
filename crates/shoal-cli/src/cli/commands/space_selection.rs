//! The spaces a command that acts on several selects, on this machine and the
//! other joined machines. Each command selects for `--all` its own way.
use anyhow::Result;
use shoal_protocol::hub::{Member, directory::Listed};
use shoal_protocol::model::Located;

use crate::{
    cli::{
        args::SpaceTargets,
        context::Context,
        ui::{self, Acting},
    },
    model::Space,
};

#[derive(Default)]
pub(super) struct Chosen {
    pub here: Vec<Space>,
    pub elsewhere: Vec<(Listed, Member)>,
}

/// With `below`, the spaces below each come too, on every machine, each once
/// and after the space above it.
pub(super) async fn located(
    ctx: &Context,
    targets: SpaceTargets,
    below: bool,
    acting: &Acting,
) -> Result<Vec<Located>> {
    if !targets.machines.is_empty() {
        return on_machines(ctx, targets.machines).await;
    }
    let selected = ui::select_spaces(ctx, targets.space, false, acting).await?;
    crate::client::spaces::select(&ctx.paths, selected, Vec::new(), below).await
}

/// Every space of the joined machines `machines` names that the caller's
/// scope reaches, this machine's but the caller's own.
pub(super) async fn on_machines(ctx: &Context, machines: Vec<String>) -> Result<Vec<Located>> {
    crate::client::spaces::select(&ctx.paths, Vec::new(), machines, false).await
}

impl Chosen {
    pub(super) fn of(located: Vec<Located>) -> Self {
        let mut chosen = Self::default();
        for located in located {
            match located {
                Located::Here(space) => chosen.here.push(*space),
                Located::Elsewhere { space, machine } => chosen.elsewhere.push((*space, *machine)),
            }
        }
        chosen
    }
}

pub(super) fn on_machine(name: &str, machine: Option<&str>) -> String {
    match machine {
        Some(machine) => format!("{name} on {machine}"),
        None => name.to_owned(),
    }
}
