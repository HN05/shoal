//! The daemon's answers to [`SimulatorMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, SimulatorMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: SimulatorMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        SimulatorMethod::SimCatalog { workspace } => {
            Body::SimCatalog(manager.simulator_catalog(&workspace).await?)
        }
        SimulatorMethod::SimOverview { workspace } => {
            Body::SimOverview(manager.simulator_overview(workspace.as_deref()).await?)
        }
        SimulatorMethod::SimList { workspace } => {
            let owner = manager.workspace_filter(workspace.as_deref()).await?;
            Body::Simulators(manager.list_simulators(owner.as_deref()).await?)
        }
        SimulatorMethod::SimAcquire { workspace, request } => manager
            .acquire_simulator(&workspace, request, caller)
            .await?
            .into_body(|sim| Body::Simulator(*sim)),
        SimulatorMethod::SimRelease { workspace, name } => {
            manager.release_simulator(&workspace, name).await?;
            Body::Ok
        }
        SimulatorMethod::SimHistory {
            workspace,
            limit,
            before,
        } => {
            let owner = manager.workspace_filter(workspace.as_deref()).await?;
            Body::SimHistory(manager.clean_history(owner, limit, before).await?)
        }
    }))
}

impl Scoped for SimulatorMethod {
    async fn scope(&mut self, _: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            SimulatorMethod::SimCatalog { workspace }
            | SimulatorMethod::SimAcquire { workspace, .. }
            | SimulatorMethod::SimRelease { workspace, .. } => Some(workspace),
            SimulatorMethod::SimOverview { workspace }
            | SimulatorMethod::SimList { workspace }
            | SimulatorMethod::SimHistory { workspace, .. } => {
                Some(workspace.get_or_insert_with(|| caller.workspace_id.clone()))
            }
        })
    }
}
