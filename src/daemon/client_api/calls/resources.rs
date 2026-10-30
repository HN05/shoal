//! Holds and simulator leases.
use shoal_client::{
    AcquireHold, AcquireSimulator, ApiError, ReleaseHold, ReleaseSimulator, SimulatorLease,
    records::Hold,
};

use super::super::{Session, errors, records};
use crate::{
    model::WorkspaceHold,
    protocol::{HoldMethod, SimulatorMethod},
    sim::{SimRequest, Simulator},
};

/// The lease name simulator requests use unless they name one.
const DEFAULT_LEASE: &str = "default";

pub(super) async fn acquire_hold(session: &Session, params: AcquireHold) -> Result<Hold, ApiError> {
    let hold: WorkspaceHold = session
        .ask(HoldMethod::HoldAcquire {
            workspace: params.workspace,
            name: params.name,
            reason: params.reason,
        })
        .await?;
    Ok(records::hold(hold))
}

pub(super) async fn release_hold(session: &Session, params: ReleaseHold) -> Result<(), ApiError> {
    session
        .ask(HoldMethod::HoldRelease {
            workspace: params.workspace,
            name: params.name,
        })
        .await
}

/// Lease a simulator, refusing with `busy` when none is free rather than
/// waiting in the simulator queue.
pub(super) async fn acquire_simulator(
    session: &Session,
    params: AcquireSimulator,
) -> Result<SimulatorLease, ApiError> {
    let request = SimRequest {
        request_id: uuid::Uuid::new_v4().to_string(),
        clean: false,
        name: params.name.unwrap_or_else(|| DEFAULT_LEASE.into()),
        profile: params.profile,
        device: params.device,
        runtime: params.runtime,
        reason: params.reason,
        queue: false,
    };
    let body = session
        .run(SimulatorMethod::SimAcquire {
            workspace: params.workspace,
            request,
        })
        .await?;
    let simulator = Simulator::try_from(errors::granted(body)?).map_err(errors::unexpected)?;
    Ok(SimulatorLease {
        name: simulator.lease_name.unwrap_or_else(|| DEFAULT_LEASE.into()),
        udid: simulator.udid,
        device: simulator.device,
        runtime: simulator.runtime,
    })
}

pub(super) async fn release_simulator(
    session: &Session,
    params: ReleaseSimulator,
) -> Result<(), ApiError> {
    session
        .ask(SimulatorMethod::SimRelease {
            workspace: params.workspace,
            name: params.name,
        })
        .await
}
