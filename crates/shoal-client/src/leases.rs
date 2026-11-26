//! Acquiring, releasing and listing the leases a space holds.
use std::time::Duration;

use anyhow::Result;
use tokio::time::sleep;

use crate::connection::{Daemon, Prepared, request};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    approvals::{ApprovalRequest, DecisionStatus},
    containers::{ContainerCatalog, ContainerOverview, Containers},
    model::{PortCatalog, PortOverview, PortRequest, PortReservation, PortSuggestion, SpaceHold},
    protocol::{
        Body, ContainerMethod, HoldMethod, PortMethod, ResourceMethod, SecretMethod,
        SimulatorMethod,
    },
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
        match RefusalCode::approval(&request) {
            RefusalCode::ApprovalDenied => Self::ApprovalDenied(request),
            _ => Self::ApprovalPending(request),
        }
    }
}

impl<T: Leased> Acquisition<T> {
    /// Why the acquisition was not granted: none once it was, nor for busy
    /// capacity of a kind without a name.
    pub fn refusal(&self) -> Option<RefusalCode> {
        match self {
            Self::Acquired(_) => None,
            Self::Busy(_) => T::BUSY,
            Self::ApprovalPending(_) => Some(RefusalCode::ApprovalPending),
            Self::ApprovalDenied(_) => Some(RefusalCode::ApprovalDenied),
        }
    }
}

shoal_states::states!(
    /// Why a request was not granted, as output names it.
    RefusalCode {
        /// A resource pool's capacity is taken.
        ResourceBusy => "resource_busy",
        /// Every simulator Shoal may run is taken.
        SimulatorBusy => "simulator_busy",
        /// The request waits for an approval.
        ApprovalPending => "approval_pending",
        /// The approval it asked for was denied.
        ApprovalDenied => "approval_denied",
    }
);

impl RefusalCode {
    /// Why a request that waits for or was denied `request` is not granted.
    pub fn approval(request: &ApprovalRequest) -> Self {
        match request.status {
            DecisionStatus::Denied => Self::ApprovalDenied,
            _ => Self::ApprovalPending,
        }
    }
}

/// What an acquisition leases, and the refusal busy capacity of its kind
/// names.
pub trait Leased {
    const BUSY: Option<RefusalCode>;
}

impl Leased for Simulator {
    const BUSY: Option<RefusalCode> = Some(RefusalCode::SimulatorBusy);
}

impl Leased for ResourceLease {
    const BUSY: Option<RefusalCode> = Some(RefusalCode::ResourceBusy);
}

impl Leased for SecretGrant {
    const BUSY: Option<RefusalCode> = None;
}

impl Leased for SpaceHold {
    const BUSY: Option<RefusalCode> = None;
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

pub fn acquire_port(
    workspace: SpaceSelector,
    name: String,
    request: PortRequest,
) -> Prepared<PortAcquisition> {
    Prepared::new(PortMethod::PortAcquire {
        workspace,
        name,
        request,
    })
}

impl TryFrom<Body> for PortAcquisition {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::Port(reservation) => Ok(Self::Reserved(reservation)),
            Body::ApprovalRequest(request) => Ok(Self::Approval(request)),
            Body::PortSuggestion(suggestion) => Ok(Self::Suggested(suggestion)),
            body => Err(body.unexpected("Port, ApprovalRequest or PortSuggestion")),
        }
    }
}

pub fn release_port(workspace: SpaceSelector, name: String) -> Prepared<()> {
    Prepared::new(PortMethod::PortRelease { workspace, name })
}

pub fn ports(workspace: SpaceSelector) -> Prepared<PortOverview> {
    Prepared::new(PortMethod::PortOverview { workspace })
}

/// The ports `workspace`'s repository configures, and the automatic range.
pub fn port_catalog(workspace: SpaceSelector) -> Prepared<PortCatalog> {
    Prepared::new(PortMethod::PortCatalog { workspace })
}

pub fn acquire_simulator(
    workspace: SpaceSelector,
    request: SimRequest,
) -> Prepared<Acquisition<Simulator>> {
    Prepared::new(SimulatorMethod::SimAcquire { workspace, request })
}

pub fn release_simulator(workspace: SpaceSelector, name: String) -> Prepared<()> {
    Prepared::new(SimulatorMethod::SimRelease { workspace, name })
}

/// The simulators `workspace` leases, or every workspace's.
pub fn simulators(workspace: Option<SpaceSelector>) -> Prepared<SimulatorOverview> {
    Prepared::new(SimulatorMethod::SimOverview { workspace })
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

/// What starting a workspace's containers came to.
pub enum ContainerAcquisition {
    Started(Box<Containers>),
    /// A port it reserves waits for, or was refused by, a decision.
    Approval(Box<ApprovalRequest>),
    /// The preferred port of one it reserves is taken; the daemon suggests
    /// another, which `acquire port` reserves.
    Suggested(PortSuggestion),
}

/// Start the containers of `workspace`, or return them when they run.
pub fn acquire_containers(
    workspace: SpaceSelector,
    reason: Option<String>,
) -> Prepared<ContainerAcquisition> {
    Prepared::new(ContainerMethod::ContainerAcquire { workspace, reason })
}

impl TryFrom<Body> for ContainerAcquisition {
    type Error = anyhow::Error;

    fn try_from(body: Body) -> Result<Self> {
        match body {
            Body::Containers(containers) => Ok(Self::Started(containers)),
            Body::ApprovalRequest(request) => Ok(Self::Approval(request)),
            Body::PortSuggestion(suggestion) => Ok(Self::Suggested(suggestion)),
            body => Err(body.unexpected("Containers, ApprovalRequest or PortSuggestion")),
        }
    }
}

/// Stop the containers of `workspace`, keeping their volumes and ports.
pub fn release_containers(workspace: SpaceSelector) -> Prepared<()> {
    Prepared::new(ContainerMethod::ContainerRelease { workspace })
}

/// Start the containers of `workspace` after its setup command, where its
/// repository starts them with setup, for the agent that starts there next
/// when `for_agent`.
pub async fn set_up_containers(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    for_agent: bool,
) -> Result<()> {
    let method = ContainerMethod::ContainerSetUp {
        workspace,
        for_agent,
    };
    request(daemon, method).await
}

/// Give the agent of `daemon`'s scope the containers of `workspace` as it
/// starts there, where its repository starts them with setup.
pub async fn start_agent_containers(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<()> {
    request(daemon, ContainerMethod::ContainerAgentStart { workspace }).await
}

/// The containers of `workspace`, or of every workspace.
pub fn containers(workspace: Option<SpaceSelector>) -> Prepared<ContainerOverview> {
    Prepared::new(ContainerMethod::ContainerOverview { workspace })
}

/// What `workspace`'s repository configures for its containers, and the
/// runtime that runs them.
pub async fn container_catalog(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
) -> Result<ContainerCatalog> {
    request(daemon, ContainerMethod::ContainerCatalog { workspace }).await
}

pub fn acquire_resource(
    workspace: SpaceSelector,
    request: ResourceRequest,
) -> Prepared<Acquisition<ResourceLease>> {
    Prepared::new(ResourceMethod::ResourceAcquire { workspace, request })
}

pub fn release_resource(workspace: SpaceSelector, pool: String, name: String) -> Prepared<()> {
    Prepared::new(ResourceMethod::ResourceRelease {
        workspace,
        pool,
        name,
    })
}

/// The resource pools `workspace` sees and the permits it holds.
pub fn resources(workspace: SpaceSelector) -> Prepared<Overview> {
    Prepared::new(ResourceMethod::ResourceOverview { workspace })
}

pub async fn resource_catalog(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<Catalog> {
    request(daemon, ResourceMethod::ResourceCatalog { workspace }).await
}

pub fn acquire_secret(
    workspace: SpaceSelector,
    name: String,
    request: SecretRequest,
) -> Prepared<Acquisition<SecretGrant>> {
    Prepared::new(SecretMethod::SecretAcquire {
        workspace,
        name,
        request,
    })
}

pub fn release_secret(workspace: SpaceSelector, name: String) -> Prepared<()> {
    Prepared::new(SecretMethod::SecretRelease { workspace, name })
}

pub async fn secret_catalog(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
) -> Result<SecretCatalog> {
    request(daemon, SecretMethod::SecretCatalog { workspace }).await
}

/// Holds `space` against cleanup.
pub fn acquire_hold(
    space: SpaceSelector,
    name: String,
    reason: Option<String>,
) -> Prepared<Acquisition<SpaceHold>> {
    Prepared::new(HoldMethod::HoldAcquire {
        space,
        name,
        reason,
    })
}

pub fn release_hold(space: SpaceSelector, name: String) -> Prepared<()> {
    Prepared::new(HoldMethod::HoldRelease { space, name })
}

pub fn holds(space: SpaceSelector) -> Prepared<Vec<SpaceHold>> {
    Prepared::new(HoldMethod::HoldList { space })
}

#[cfg(test)]
mod tests;
