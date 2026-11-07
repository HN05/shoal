//! Leases a workspace holds: acquiring, releasing and listing its ports,
//! simulators, resource permits, secret grants and holds.
use std::time::Duration;

use anyhow::Result;
use tokio::time::sleep;

use crate::space_names::SpaceSelector;
use crate::{
    approvals::{ApprovalRequest, DecisionStatus},
    connection::{Daemon, call, request},
    model::{
        PortCatalog, PortOverview, PortRequest, PortReservation, PortSuggestion, WorkspaceHold,
    },
    protocol::{Body, HoldMethod, PortMethod, ResourceMethod, SecretMethod, SimulatorMethod},
    resources::{Catalog, Overview, ResourceLease, ResourceRequest},
    secrets::{SecretCatalog, SecretGrant, SecretRequest},
    sim::{AuditEntry, SimRequest, Simulator, SimulatorCatalog, SimulatorOverview},
};

/// What an acquisition came to.
#[derive(Debug)]
pub enum Acquisition<T> {
    Acquired(T),
    /// Capacity is taken; the message says by what.
    Busy(String),
    ApprovalPending(Box<ApprovalRequest>),
    ApprovalDenied(Box<ApprovalRequest>),
}

impl<T> Acquisition<T> {
    fn approval(request: Box<ApprovalRequest>) -> Self {
        if request.status == DecisionStatus::Denied {
            Self::ApprovalDenied(request)
        } else {
            Self::ApprovalPending(request)
        }
    }
}

impl<T: TryFrom<Body, Error = anyhow::Error>> TryFrom<Body> for Acquisition<T> {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::ApprovalRequest(request) => Ok(Self::approval(request)),
            Body::Busy { message } => Ok(Self::Busy(message)),
            body => T::try_from(body).map(Self::Acquired),
        }
    }
}

/// Without a queue, return the first outcome. A queued request polls capacity
/// and pending approvals about once a second, keeping its place, until
/// acquisition, denial or an error.
pub async fn retry<T>(
    queue: bool,
    mut attempt: impl AsyncFnMut() -> Result<Acquisition<T>>,
) -> Result<Acquisition<T>> {
    loop {
        let outcome = attempt().await?;
        match outcome {
            Acquisition::Busy(_) | Acquisition::ApprovalPending(_) if queue => {
                sleep(Duration::from_secs(1)).await;
            }
            _ => return Ok(outcome),
        }
    }
}

/// What a port reservation came to.
pub enum PortAcquisition {
    Reserved(PortReservation),
    /// The request waits for, or was refused by, a decision.
    Approval(Box<ApprovalRequest>),
    /// The preferred port is taken; the daemon suggests another.
    Suggested(PortSuggestion),
}

/// Reserve the port `name` for `workspace`.
pub async fn acquire_port(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
    request: PortRequest,
) -> Result<PortAcquisition> {
    let method = PortMethod::PortAcquire {
        workspace,
        name,
        request,
    };
    match call(daemon, method).await? {
        Body::Port(reservation) => Ok(PortAcquisition::Reserved(reservation)),
        Body::ApprovalRequest(request) => Ok(PortAcquisition::Approval(request)),
        Body::PortSuggestion(suggestion) => Ok(PortAcquisition::Suggested(suggestion)),
        body => Err(body.unexpected("Port, ApprovalRequest or PortSuggestion")),
    }
}

pub async fn release_port(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
) -> Result<()> {
    request(daemon, PortMethod::PortRelease { workspace, name }).await
}

/// The ports `workspace` reserved.
pub async fn ports(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<PortOverview> {
    request(daemon, PortMethod::PortOverview { workspace }).await
}

/// The ports `workspace`'s repository configures, and the automatic range.
pub async fn port_catalog(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<PortCatalog> {
    request(daemon, PortMethod::PortCatalog { workspace }).await
}

pub async fn acquire_simulator(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    request: SimRequest,
) -> Result<Acquisition<Simulator>> {
    self::request(daemon, SimulatorMethod::SimAcquire { workspace, request }).await
}

pub async fn release_simulator(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
) -> Result<()> {
    request(daemon, SimulatorMethod::SimRelease { workspace, name }).await
}

/// The simulators `workspace` leases, or every workspace's.
pub async fn simulators(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
) -> Result<SimulatorOverview> {
    request(daemon, SimulatorMethod::SimOverview { workspace }).await
}

/// The simulator devices `workspace` sees, or every one.
pub async fn simulator_devices(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
) -> Result<Vec<Simulator>> {
    request(daemon, SimulatorMethod::SimList { workspace }).await
}

/// The simulator profiles and runtimes `workspace` may lease.
pub async fn simulator_catalog(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
) -> Result<SimulatorCatalog> {
    request(daemon, SimulatorMethod::SimCatalog { workspace }).await
}

/// Up to `limit` audited simulator changes of `workspace`, or of every
/// workspace, older than the audit ID `before`.
pub async fn simulator_history(
    daemon: &impl Daemon,
    workspace: Option<SpaceSelector>,
    limit: u32,
    before: Option<i64>,
) -> Result<Vec<AuditEntry>> {
    let method = SimulatorMethod::SimHistory {
        workspace,
        limit,
        before,
    };
    request(daemon, method).await
}

pub async fn acquire_resource(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    request: ResourceRequest,
) -> Result<Acquisition<ResourceLease>> {
    self::request(
        daemon,
        ResourceMethod::ResourceAcquire { workspace, request },
    )
    .await
}

pub async fn release_resource(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    pool: String,
    name: String,
) -> Result<()> {
    let method = ResourceMethod::ResourceRelease {
        workspace,
        pool,
        name,
    };
    request(daemon, method).await
}

/// The resource pools `workspace` sees and the permits it holds.
pub async fn resources(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Overview> {
    request(daemon, ResourceMethod::ResourceOverview { workspace }).await
}

/// The resource pools `workspace`'s configuration defines.
pub async fn resource_catalog(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Catalog> {
    request(daemon, ResourceMethod::ResourceCatalog { workspace }).await
}

pub async fn acquire_secret(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
    request: SecretRequest,
) -> Result<Acquisition<SecretGrant>> {
    let method = SecretMethod::SecretAcquire {
        workspace,
        name,
        request,
    };
    self::request(daemon, method).await
}

pub async fn release_secret(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
) -> Result<()> {
    request(daemon, SecretMethod::SecretRelease { workspace, name }).await
}

/// The secrets `workspace`'s configuration defines.
pub async fn secret_catalog(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
) -> Result<SecretCatalog> {
    request(daemon, SecretMethod::SecretCatalog { workspace }).await
}

/// Hold `workspace` against cleanup as `name`, for `reason`.
pub async fn acquire_hold(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
    reason: Option<String>,
) -> Result<Acquisition<WorkspaceHold>> {
    let method = HoldMethod::HoldAcquire {
        workspace,
        name,
        reason,
    };
    request(daemon, method).await
}

pub async fn release_hold(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    name: String,
) -> Result<()> {
    request(daemon, HoldMethod::HoldRelease { workspace, name }).await
}

/// The holds on `workspace`.
pub async fn holds(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Vec<WorkspaceHold>> {
    request(daemon, HoldMethod::HoldList { workspace }).await
}

#[cfg(test)]
mod tests;
