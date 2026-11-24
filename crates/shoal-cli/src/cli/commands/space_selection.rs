//! The spaces a command that acts on several selects, on this machine and
//! on the other joined machines: those its names select wherever they
//! run, every space of the machines `--machine` names, or those picked.
//! Each command selects for `--all` its own way, on this machine.
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

/// The spaces a command selects, by the machine each runs on: this
/// machine's, and other machines' with the machine of each.
#[derive(Default)]
pub(super) struct Chosen {
    pub here: Vec<Space>,
    pub elsewhere: Vec<(Listed, Member)>,
}

/// The spaces `targets` selects but for `--all`: with `--machine` every
/// space of those machines the caller's scope reaches, else those its
/// names select wherever they run, or those picked among the spaces the
/// request `acting` sends takes, with the spaces below each on every
/// machine when `below`, each once and after the space above it.
pub(super) async fn located(
    ctx: &Context,
    targets: SpaceTargets,
    below: bool,
    acting: &Acting,
) -> Result<Vec<Located>> {
    let SpaceTargets {
        space, machines, ..
    } = targets;
    let selected = match machines.is_empty() {
        true => ui::select_spaces(ctx, space, false, acting).await?,
        false => Vec::new(),
    };
    crate::client::spaces::select(&ctx.paths, selected, machines, below).await
}

impl Chosen {
    /// The spaces `located`, which the daemon selected each once, by the
    /// machine each runs on.
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
