//! Lease commands: the request each sends for the space it acts on.
use crate::cli::{
    args::resources::{HistoryKind, LeaseKind, ResourcesCommand},
    commands::{catalog, containers, holds, leases, ports, resources, simulators},
    ui::Acting,
};

/// What a lease command sends for the space it acts on.
pub(super) fn acting(command: &ResourcesCommand) -> Option<Acting> {
    Some(match command {
        ResourcesCommand::Acquire { kind, .. } => leases::acquiring(kind),
        ResourcesCommand::Release { kind, .. } => leases::releasing(kind.as_ref()),
        ResourcesCommand::Leases { kind, .. } => match kind {
            Some(LeaseKind::Hold) => holds::listing(),
            Some(LeaseKind::Port) | None => ports::listing(),
            Some(LeaseKind::Sim) => simulators::listing(),
            Some(LeaseKind::Containers) => containers::listing(),
            Some(LeaseKind::Resource | LeaseKind::Repo) => resources::listing(),
        },
        ResourcesCommand::Catalog { .. } => catalog::reading(),
        ResourcesCommand::History {
            kind: HistoryKind::Sim { .. },
        } => simulators::listing(),
    })
}
