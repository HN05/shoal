use anyhow::{Context, Result, bail, ensure};
use rusqlite::{TransactionBehavior, params};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr},
};

use crate::{model::PortReservation, store, workspace::Manager};

fn available(port: u16) -> Result<bool> {
    // No SO_REUSEADDR: a standard TcpListener enables it and can miss a
    // loopback listener when probing a wildcard address on macOS.
    for address in [
        std::net::SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        std::net::SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
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

impl Manager {
    pub async fn reserve_port(
        &self,
        selector: String,
        name: String,
        requested: Option<u16>,
        env_var: Option<String>,
        reason: Option<String>,
    ) -> Result<PortReservation> {
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name.as_bytes()[0].is_ascii_lowercase()
                && name.bytes().all(|c| c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || c == b'_'
                    || c == b'-'),
            "port names must start with a lowercase letter and contain only lowercase letters, digits, _ or - (max 64)"
        );
        ensure!(requested != Some(0), "port zero cannot be reserved");
        ensure!(
            reason.as_ref().is_none_or(|text| !text.trim().is_empty()
                && text.len() <= 256
                && !text.contains(['\n', '\r'])),
            "port reason must be a nonempty single line (max 256 bytes)"
        );
        let default_env = format!("SHOAL_PORT_{}", name.replace('-', "_").to_ascii_uppercase());
        if let Some(env) = &env_var {
            ensure!(
                !env.is_empty()
                    && (env.as_bytes()[0].is_ascii_uppercase() || env.starts_with('_'))
                    && env
                        .bytes()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_'),
                "invalid port environment variable"
            );
            ensure!(
                !matches!(env.as_str(), "HOME" | "PATH" | "SHELL" | "TMPDIR")
                    && (!env.starts_with("SHOAL_") || env == &default_env),
                "port environment variable conflicts with the execution environment"
            );
        }
        let workspace = self.get(selector).await?;
        self.touch(&workspace.id).await;
        let range = self.config.ports;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let ready: bool = tx.query_row(
                    "SELECT state='ready' FROM workspaces WHERE id=?1",
                    [&workspace.id],
                    |row| row.get(0),
                )?;
                ensure!(ready, "workspace is not ready");
                let existing = store::ports(&tx, Some(&workspace.id))?;
                if let Some(reservation) = existing.iter().find(|p| p.name == name) {
                    ensure!(
                        requested.is_none_or(|port| port == reservation.port)
                            && env_var
                                .as_ref()
                                .is_none_or(|env| env == &reservation.env_var),
                        "port name already reserved with different settings; release it first"
                    );
                    let mut reservation = reservation.clone();
                    if let Some(reason) = reason {
                        tx.execute("UPDATE ports SET reason=?3 WHERE workspace_id=?1 AND name=?2", params![workspace.id,name,reason])?;
                        reservation.reason = Some(reason);
                    }
                    tx.commit()?;
                    return Ok(reservation);
                }
                let env_var = env_var.unwrap_or(default_env);
                ensure!(
                    !existing.iter().any(|p| p.env_var == env_var),
                    "environment variable already assigned to another port"
                );
                let reserved = store::ports(&tx, None)?;
                let candidates = match requested {
                    Some(port) => port..=port,
                    None => range.start..=range.end,
                };
                let mut selected = None;
                for port in candidates {
                    if !reserved.iter().any(|p| p.port == port) && available(port)? {
                        selected = Some(port);
                        break;
                    }
                }
                let Some(port) = selected else {
                    if let Some(port) = requested {
                        bail!("TCP port {port} is reserved or unavailable");
                    }
                    bail!("no available TCP ports in {}..={}", range.start, range.end);
                };
                tx.execute(
                    "INSERT INTO ports (workspace_id,name,port,env_var,reason) VALUES (?1,?2,?3,?4,?5)",
                    params![workspace.id, name, port, env_var, reason],
                )?;
                tx.commit()?;
                Ok(PortReservation {
                    workspace_id: workspace.id,
                    name,
                    port,
                    env_var,
                    reason,
                })
            })
            .await
    }

    pub async fn list_ports(&self, selector: Option<String>) -> Result<Vec<PortReservation>> {
        let workspace_id = match selector {
            Some(selector) => Some(self.get(selector).await?.id),
            None => None,
        };
        self.store
            .run(move |db| store::ports(db, workspace_id.as_deref()))
            .await
    }

    pub async fn release_port(&self, selector: String, name: String) -> Result<()> {
        let workspace = self.get(selector).await?;
        self.touch(&workspace.id).await;
        self.store
            .run(move |db| {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let ready: bool = tx.query_row(
                    "SELECT state='ready' FROM workspaces WHERE id=?1",
                    [&workspace.id],
                    |row| row.get(0),
                )?;
                ensure!(ready, "workspace is not ready");
                ensure!(
                    tx.execute(
                        "DELETE FROM ports WHERE workspace_id=?1 AND name=?2",
                        params![workspace.id, name]
                    )? == 1,
                    "unknown port reservation"
                );
                tx.commit()?;
                Ok(())
            })
            .await
    }
}
