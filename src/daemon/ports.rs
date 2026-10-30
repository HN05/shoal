//! Cooperative TCP port reservations owned by a worktree.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::RangeInclusive,
};

use crate::{
    config::{Ports, repo::ConflictPolicy},
    daemon::{
        allocation::Allocation,
        approvals::{self, ApprovalRequest, Lifetime, PortSpecification, Specification, Target},
        notifications::NotificationKind,
        scope::Caller,
        store,
        workspace::Manager,
    },
    env,
    model::{PortCatalog, PortOverview, PortReservation, PortSuggestion},
    protocol::Body,
    validate,
};

mod selection;
#[cfg(test)]
mod tests;

use selection::first_available;

/// Caller overrides for a named port; unset fields fall back to the
/// repository's port definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortRequest {
    pub port: Option<u16>,
    pub env_var: Option<String>,
    pub reason: Option<String>,
    pub on_conflict: Option<ConflictPolicy>,
}

pub enum Acquisition {
    Allocation(Allocation<PortReservation>),
    /// The preferred port is taken and policy asks the caller to confirm.
    Suggested(PortSuggestion),
}

impl Acquisition {
    /// The reply to a port acquisition.
    pub fn into_body(self) -> Body {
        match self {
            Self::Allocation(allocation) => allocation.into_body(Body::Port),
            Self::Suggested(suggestion) => Body::PortSuggestion(suggestion),
        }
    }
}

/// Probe the port on both wildcard addresses without SO_REUSEADDR: a standard
/// TcpListener enables it and can miss a loopback listener on macOS.
fn available(port: u16) -> Result<bool> {
    for address in [
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
    ] {
        let probe = || -> io::Result<()> {
            let domain = if address.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            };
            let socket =
                socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
            if address.is_ipv6() {
                socket.set_only_v6(true)?;
            }
            socket.bind(&address.into())?;
            socket.listen(1)?;
            Ok(())
        };
        match probe() {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok(false);
            }
            Err(error)
                if address.is_ipv6()
                    && (matches!(
                        error.kind(),
                        io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
                    ) || error.raw_os_error() == Some(libc::EAFNOSUPPORT)) => {}
            Err(error) => return Err(error).context("probe TCP port"),
        }
    }
    // The sockets are released so applications can bind their allocated ports.
    Ok(true)
}

/// `SHOAL_PORT_<NAME>`, the variable a reservation exports unless overridden.
fn default_env_var(name: &str) -> String {
    format!(
        "{}{}",
        env::PORT_PREFIX,
        name.replace('-', "_").to_ascii_uppercase()
    )
}

fn validate_env_var(env_var: &str, default: &str) -> Result<()> {
    ensure!(
        !env_var.is_empty()
            && (env_var.as_bytes()[0].is_ascii_uppercase() || env_var.starts_with('_'))
            && env_var
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'),
        "invalid port environment variable"
    );
    ensure!(
        !env::PROTECTED.contains(&env_var)
            && (!env_var.starts_with(env::PREFIX) || env_var == default),
        "port environment variable conflicts with the execution environment"
    );
    Ok(())
}

impl Manager {
    pub async fn acquire_port(
        &self,
        selector: &str,
        name: String,
        request: PortRequest,
        caller: Option<&Caller>,
    ) -> Result<Acquisition> {
        let workspace = self.workspace(selector).await?;
        let ports = self
            .workspace_settings(&workspace)
            .await?
            .into_settings()
            .ports;
        let claim =
            PortClaim::resolve(workspace.id.clone(), name, request, ports, caller.is_some())?;
        self.touch(&workspace.id).await;
        let (outcome, conflict) = self.store.run(move |db| claim_port(db, claim)).await?;
        if let Some(conflict) = conflict {
            self.notify(
                Some(&workspace.name),
                NotificationKind::PortConflict,
                conflict,
            )
            .await;
        }
        if let Acquisition::Allocation(allocation) = &outcome {
            self.notify_allocation(&workspace.name, allocation, "")
                .await;
        }
        Ok(outcome)
    }

    pub async fn port_overview(&self, selector: &str) -> Result<PortOverview> {
        let workspace = self.workspace(selector).await?;
        let reserved = self.list_ports(Some(&workspace.id)).await?;
        Ok(PortOverview {
            workspace,
            reserved,
        })
    }

    pub async fn port_catalog(&self, selector: &str) -> Result<PortCatalog> {
        let workspace = self.workspace(selector).await?;
        let ports = self
            .workspace_settings(&workspace)
            .await?
            .into_settings()
            .ports;
        Ok(PortCatalog {
            workspace,
            start: ports.start,
            end: ports.end,
            on_conflict: ports.on_conflict,
            configured: ports.definitions,
        })
    }

    pub async fn list_ports(&self, selector: Option<&str>) -> Result<Vec<PortReservation>> {
        let workspace_id = self.workspace_filter(selector).await?;
        self.store
            .run(move |db| store::ports(db, workspace_id.as_deref()))
            .await
    }

    pub async fn release_port(&self, selector: &str, name: String) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.touch(&workspace.id).await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::require_ready(&tx, &workspace.id)?;
                let released =
                    approvals::release(&tx, &workspace.id, &Target::Port(name.clone()), &name)?;
                ensure!(
                    tx.execute(
                        "DELETE FROM ports WHERE workspace_id=?1 AND name=?2",
                        params![workspace.id, name]
                    )? == 1
                        || released,
                    "unknown port reservation or approval request"
                );
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

/// A named port request resolved against the repository's port settings:
/// caller overrides win over the port's definition, then the range defaults.
struct PortClaim {
    workspace_id: String,
    name: String,
    /// The caller's explicit settings, which an existing reservation must match.
    request: PortRequest,
    preferred: Option<u16>,
    env_var: String,
    reason: Option<String>,
    policy: ConflictPolicy,
    range: RangeInclusive<u16>,
    /// Set when the caller is scoped and the port requires approval.
    approval: Option<Lifetime>,
}

impl PortClaim {
    fn resolve(
        workspace_id: String,
        name: String,
        request: PortRequest,
        ports: Ports,
        scoped: bool,
    ) -> Result<Self> {
        validate::lowercase_name("port", &name)?;
        let definition = ports.definitions.get(&name).cloned().unwrap_or_default();
        let preferred = request.port.or(definition.port);
        ensure!(preferred != Some(0), "port zero cannot be reserved");
        let reason = request.reason.clone().or(definition.reason);
        validate::reason("port", reason.as_deref())?;
        let default_env = default_env_var(&name);
        let env_var = match request.env_var.clone().or(definition.env) {
            Some(env_var) => {
                validate_env_var(&env_var, &default_env)?;
                env_var
            }
            None => default_env,
        };
        let policy = request
            .on_conflict
            .or(definition.on_conflict)
            .unwrap_or(ports.on_conflict);
        Ok(Self {
            workspace_id,
            name,
            request,
            preferred,
            env_var,
            reason,
            policy,
            range: ports.start..=ports.end,
            approval: (scoped && definition.requires_approval)
                .then_some(definition.approval_lifetime),
        })
    }

    fn conflict(&self, requested: u16, outcome: &str) -> String {
        format!("port {}: {requested} is in use; {outcome}", self.name)
    }
}

/// The port a new reservation takes, or the free port it may only suggest
/// when its preferred port is taken.
enum Selection {
    Reserve(u16),
    Suggest { requested: u16, suggested: u16 },
}

/// Claim or renew atomically, returning the outcome and any port conflict to
/// report. Grants and approval requests commit; a suggestion changes nothing.
fn claim_port(db: &mut Connection, claim: PortClaim) -> Result<(Acquisition, Option<String>)> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    store::require_ready(&tx, &claim.workspace_id)?;
    let existing = store::ports(&tx, Some(&claim.workspace_id))?;
    if let Some(reservation) = existing.iter().find(|p| p.name == claim.name) {
        let renewed = renew_reservation(tx, reservation, claim.request)?;
        return Ok((Acquisition::Allocation(Allocation::Granted(renewed)), None));
    }
    ensure!(
        !existing.iter().any(|p| p.env_var == claim.env_var),
        "environment variable already assigned to another port"
    );
    if let Some(lifetime) = claim.approval {
        let bound = PortSpecification {
            env: claim.env_var.clone(),
            on_conflict: claim.policy,
            preferred: claim.preferred,
            range: [*claim.range.start(), *claim.range.end()],
        };
        let approval = ApprovalRequest::new(
            &claim.workspace_id,
            Target::Port(claim.name.clone()),
            &claim.name,
            Specification::Port(bound),
            lifetime,
            claim.request.reason.as_deref(),
        );
        if let Some(approval) = approvals::check(&tx, approval)? {
            tx.commit()?;
            return Ok((
                Acquisition::Allocation(Allocation::Approval(Box::new(approval))),
                None,
            ));
        }
    }
    let port = match select_port(&tx, &claim)? {
        Selection::Reserve(port) => port,
        Selection::Suggest {
            requested,
            suggested,
        } => {
            let conflict = claim.conflict(requested, &format!("suggested {suggested}"));
            let suggestion = PortSuggestion {
                workspace_id: claim.workspace_id,
                name: claim.name,
                requested_port: requested,
                suggested_port: suggested,
                env_var: claim.env_var,
                reason: claim.reason,
            };
            return Ok((Acquisition::Suggested(suggestion), Some(conflict)));
        }
    };
    let conflict = claim
        .preferred
        .filter(|requested| *requested != port)
        .map(|requested| claim.conflict(requested, &format!("reserved {port} instead")));
    let reservation = PortReservation {
        workspace_id: claim.workspace_id,
        name: claim.name,
        port,
        env_var: claim.env_var,
        reason: claim.reason,
    };
    insert_reservation(&tx, &reservation)?;
    tx.commit()?;
    Ok((
        Acquisition::Allocation(Allocation::Granted(reservation)),
        conflict,
    ))
}

/// An existing reservation with the same name is returned again, refreshing
/// its reason, as long as the request does not contradict it.
fn renew_reservation(
    tx: Transaction<'_>,
    existing: &PortReservation,
    request: PortRequest,
) -> Result<PortReservation> {
    ensure!(
        request.port.is_none_or(|port| port == existing.port)
            && request
                .env_var
                .as_ref()
                .is_none_or(|env| env == &existing.env_var),
        "port name already reserved with different settings; release it first"
    );
    let mut reservation = existing.clone();
    if let Some(reason) = request.reason {
        tx.execute(
            "UPDATE ports SET reason=?3 WHERE workspace_id=?1 AND name=?2",
            params![reservation.workspace_id, reservation.name, reason],
        )?;
        reservation.reason = Some(reason);
    }
    tx.commit()?;
    Ok(reservation)
}

/// The preferred port when no reservation holds it and it is free, otherwise
/// the first free port in the range, which the conflict policy may only suggest.
fn select_port(tx: &Transaction<'_>, claim: &PortClaim) -> Result<Selection> {
    let reserved = store::ports(tx, None)?;
    if let Some(port) = claim.preferred
        && !reserved.iter().any(|p| p.port == port)
        && available(port)?
    {
        return Ok(Selection::Reserve(port));
    }
    let Some(port) = first_available(&reserved, claim.range.clone(), available)? else {
        bail!(
            "no available TCP ports in {}..={}",
            claim.range.start(),
            claim.range.end()
        );
    };
    Ok(match (claim.preferred, claim.policy) {
        (Some(requested), ConflictPolicy::Suggest) => Selection::Suggest {
            requested,
            suggested: port,
        },
        _ => Selection::Reserve(port),
    })
}

fn insert_reservation(tx: &Transaction<'_>, reservation: &PortReservation) -> Result<()> {
    tx.execute(
        "INSERT INTO ports (workspace_id,name,port,env_var,reason) VALUES (?1,?2,?3,?4,?5)",
        params![
            reservation.workspace_id,
            reservation.name,
            reservation.port,
            reservation.env_var,
            reservation.reason
        ],
    )?;
    Ok(())
}
