//! Cooperative TCP port reservations owned by a worktree.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
};

use crate::{
    config::repo::ConflictPolicy,
    daemon::{
        access::{self, AccessRequest, PortSpecification, Specification, Target},
        allocation::Allocation,
        notifications::NotificationKind,
        scope::Caller,
        store,
        workspace::Manager,
    },
    env,
    model::{PortOverview, PortReservation, PortSuggestion},
    validate,
};

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
        let range = self.workspace_settings(&workspace).await?.ports;
        let definition = range.definitions.get(&name).cloned().unwrap_or_default();
        let preferred = request.port.or(definition.port);
        let env_var = request.env_var.clone().or(definition.env);
        let reason = request.reason.clone().or(definition.reason);
        let policy = request
            .on_conflict
            .or(definition.on_conflict)
            .unwrap_or(range.on_conflict);
        validate::lowercase_name("port", &name)?;
        ensure!(preferred != Some(0), "port zero cannot be reserved");
        validate::reason("port", reason.as_deref())?;
        let default_env = default_env_var(&name);
        if let Some(env_var) = &env_var {
            validate_env_var(env_var, &default_env)?;
        }
        self.touch(&workspace.id).await;
        let workspace_name = workspace.name.clone();
        let scoped = caller.is_some();
        let (outcome, conflict) = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                store::require_ready(&tx, &workspace.id)?;
                let existing = store::ports(&tx, Some(&workspace.id))?;
                if let Some(reservation) = existing.iter().find(|p| p.name == name) {
                    ensure!(
                        request.port.is_none_or(|port| port == reservation.port)
                            && request
                                .env_var
                                .as_ref()
                                .is_none_or(|env| env == &reservation.env_var),
                        "port name already reserved with different settings; release it first"
                    );
                    let mut reservation = reservation.clone();
                    if let Some(reason) = request.reason {
                        tx.execute(
                            "UPDATE ports SET reason=?3 WHERE workspace_id=?1 AND name=?2",
                            params![workspace.id, name, reason],
                        )?;
                        reservation.reason = Some(reason);
                    }
                    tx.commit()?;
                    return Ok((Acquisition::Allocation(Allocation::Granted(reservation)), None));
                }
                let env_var = env_var.unwrap_or(default_env);
                ensure!(
                    !existing.iter().any(|p| p.env_var == env_var),
                    "environment variable already assigned to another port"
                );
                if scoped && definition.requires_approval {
                    let bound = PortSpecification { env: env_var.clone(), on_conflict: policy, preferred,
                        range: [range.start, range.end] };
                    let approval = AccessRequest::new(&workspace.id, Target::Port(name.clone()), &name,
                        Specification::Port(bound), definition.approval_lifetime, request.reason.as_deref());
                    if let Some(approval) = access::check(&tx, approval)? {
                        tx.commit()?;
                        return Ok((Acquisition::Allocation(Allocation::Approval(Box::new(approval))), None));
                    }
                }
                let reserved = store::ports(&tx, None)?;
                let free = |port| -> Result<bool> {
                    Ok(!reserved.iter().any(|p| p.port == port) && available(port)?)
                };
                let port = match preferred {
                    Some(port) if free(port)? => port,
                    _ => {
                        let mut selected = None;
                        for port in range.start..=range.end {
                            if free(port)? {
                                selected = Some(port);
                                break;
                            }
                        }
                        let Some(port) = selected else {
                            bail!(
                                "no available TCP ports in {}..={}",
                                range.start,
                                range.end
                            );
                        };
                        if let Some(requested_port) = preferred
                            && matches!(policy, ConflictPolicy::Suggest)
                        {
                            let conflict =
                                format!("port {name}: {requested_port} is in use; suggested {port}");
                            return Ok((
                                Acquisition::Suggested(PortSuggestion {
                                    workspace_id: workspace.id,
                                    name,
                                    requested_port,
                                    suggested_port: port,
                                    env_var,
                                    reason,
                                }),
                                Some(conflict),
                            ));
                        }
                        port
                    }
                };
                let conflict = preferred.filter(|requested| *requested != port).map(|requested| {
                    format!("port {name}: {requested} is in use; reserved {port} instead")
                });
                tx.execute(
                    "INSERT INTO ports (workspace_id,name,port,env_var,reason) VALUES (?1,?2,?3,?4,?5)",
                    params![workspace.id, name, port, env_var, reason],
                )?;
                tx.commit()?;
                Ok((
                    Acquisition::Allocation(Allocation::Granted(PortReservation {
                        workspace_id: workspace.id,
                        name,
                        port,
                        env_var,
                        reason,
                    })),
                    conflict,
                ))
            })
            .await?;
        if let Some(conflict) = conflict {
            self.notify(
                Some(&workspace_name),
                NotificationKind::PortConflict,
                conflict,
            )
            .await;
        }
        if let Acquisition::Allocation(allocation) = &outcome {
            self.notify_allocation(&workspace_name, allocation, "")
                .await;
        }
        Ok(outcome)
    }

    pub async fn port_overview(&self, selector: &str) -> Result<PortOverview> {
        let workspace = self.workspace(selector).await?;
        let ports = self.workspace_settings(&workspace).await?.ports;
        let reserved = self.list_ports(Some(&workspace.id)).await?;
        Ok(PortOverview {
            workspace,
            reserved,
            configured: ports.definitions,
            on_conflict: ports.on_conflict,
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
                    access::release(&tx, &workspace.id, &Target::Port(name.clone()), &name)?;
                ensure!(
                    tx.execute(
                        "DELETE FROM ports WHERE workspace_id=?1 AND name=?2",
                        params![workspace.id, name]
                    )? == 1
                        || released,
                    "unknown port reservation or access request"
                );
                tx.commit()?;
                Ok(())
            })
            .await
    }
}
