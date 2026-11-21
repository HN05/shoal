//! The daemon's answers to [`SimulatorMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::protocol::{Body, SimulatorMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: SimulatorMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        SimulatorMethod::SimCatalog { workspace } => Body::SimCatalog(
            manager
                .leases
                .simulator_catalog(&**manager, &workspace)
                .await?,
        ),
        SimulatorMethod::SimOverview { workspace } => Body::SimOverview(
            manager
                .leases
                .simulator_overview(&**manager, workspace.as_ref())
                .await?,
        ),
        SimulatorMethod::SimList { workspace } => Body::Simulators(
            manager
                .leases
                .selected_simulators(&**manager, workspace.as_ref())
                .await?,
        ),
        SimulatorMethod::SimAcquire { workspace, request } => manager
            .leases
            .acquire_simulator(&**manager, &workspace, request, caller)
            .await?
            .into_body(|sim| Body::Simulator(*sim)),
        SimulatorMethod::SimRelease { workspace, name } => {
            manager
                .leases
                .release_simulator(&**manager, &workspace, name)
                .await?;
            Body::Ok
        }
        SimulatorMethod::SimHistory {
            workspace,
            limit,
            before,
        } => Body::SimHistory(
            manager
                .leases
                .clean_history(&**manager, workspace.as_ref(), limit, before)
                .await?,
        ),
    }))
}

impl Checked for SimulatorMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SimAcquire { .. } | Self::SimRelease { .. } => Some(TargetCheck::ReadyWorkspace),
            Self::SimCatalog { .. }
            | Self::SimOverview { .. }
            | Self::SimList { .. }
            | Self::SimHistory { .. } => Some(TargetCheck::Workspace),
        }
    }
}

impl Scoped for SimulatorMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            SimulatorMethod::SimCatalog { workspace }
            | SimulatorMethod::SimAcquire { workspace, .. }
            | SimulatorMethod::SimRelease { workspace, .. } => Some(workspace),
            SimulatorMethod::SimOverview { workspace }
            | SimulatorMethod::SimList { workspace }
            | SimulatorMethod::SimHistory { workspace, .. } => {
                Some(caller.own_by_default(workspace))
            }
        })
    }
}
