//! Which forge each host serves: the type the global config pins, a
//! well-known name, the answer the host gave when last asked, or else a new
//! answer, which is recorded so the host is asked once.
use std::future::Future;

use anyhow::{Result, anyhow, bail, ensure};
use rusqlite::{OptionalExtension, params};

use crate::{
    config::forges::{self, Forges},
    daemon::{log, workspace::Manager},
    forge::{
        ForgeRepo,
        backends::ForgeKind,
        hosts::{self, HostType, HostTypes},
    },
};

/// The daemon's answers, asking new hosts with the user's logins.
pub(crate) struct ForgeHosts<'a>(&'a Manager);

impl HostTypes for ForgeHosts<'_> {
    fn forge_type(
        &self,
        host: &str,
        scheme: &'static str,
    ) -> impl Future<Output = Result<HostType>> + Send {
        self.0.host_type(host, scheme)
    }
}

/// A recorded answer: the host, its forge, and the scheme it was asked over.
type Recorded = (String, ForgeKind, String);

/// A recorded host that now serves another forge, which Shoal uses from now on.
pub(crate) struct Change {
    from: ForgeKind,
    to: ForgeKind,
}

impl Change {
    /// What changed for `host`, for the user.
    pub(crate) fn message(&self, host: &str) -> String {
        let Self { from, to } = self;
        format!("{host} now answers as {to} instead of {from}; Shoal uses {to} from now on")
    }
}

impl Manager {
    pub(crate) fn forge_hosts(&self) -> ForgeHosts<'_> {
        ForgeHosts(self)
    }

    /// The repository `remote` names, on the forge its host serves.
    pub(crate) async fn forge(&self, remote: &str) -> Result<ForgeRepo> {
        ForgeRepo::resolve(remote, &self.forge_hosts()).await
    }

    /// The forge a PR URL's repository is on.
    pub(crate) async fn pull_forge(&self, url: &str) -> Result<ForgeRepo> {
        ForgeRepo::resolve_pull_url(url, &self.forge_hosts()).await
    }

    /// The forge a CI run URL's repository is on.
    pub(crate) async fn run_url_forge(&self, url: &str) -> Result<ForgeRepo> {
        ForgeRepo::resolve_run_url(url, &self.forge_hosts()).await
    }

    /// The forge an issue URL's repository is on.
    pub(crate) async fn issue_url_forge(&self, url: &str) -> Result<ForgeRepo> {
        ForgeRepo::resolve_issue_url(url, &self.forge_hosts()).await
    }

    /// The forge `host` serves and what decided it, asking it over
    /// `scheme` when nothing says.
    pub(crate) async fn host_type(&self, host: &str, scheme: &'static str) -> Result<HostType> {
        if let Some(kind) = self.config().forges.settled(host) {
            return Ok(HostType::Settled(kind));
        }
        if let Some(kind) = self.recorded_forge_type(host).await? {
            return Ok(HostType::Answered(kind));
        }
        // Another lookup may have asked the host while this one waited.
        let _gate = self.gates.lock_forge_host(host).await;
        if let Some(kind) = self.recorded_forge_type(host).await? {
            return Ok(HostType::Answered(kind));
        }
        Ok(HostType::Answered(self.ask_forge_type(host, scheme).await?))
    }

    /// The forge type a request names for `host` over `scheme`, which the
    /// caller does not supply as a static string.
    pub(crate) async fn requested_forge_type(&self, host: &str, scheme: &str) -> Result<ForgeKind> {
        let scheme = match scheme {
            "http" => "http",
            "https" => "https",
            other => bail!("unsupported forge scheme {other:?}"),
        };
        ensure!(
            !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')),
            "invalid forge host {host:?}"
        );
        Ok(self
            .host_type(&host.to_ascii_lowercase(), scheme)
            .await?
            .kind())
    }

    /// Remember the recorded answers for this process, as the daemon starts,
    /// so links of those hosts parse as their forges before a lookup.
    pub(crate) async fn learn_recorded_forge_hosts(&self) -> Result<()> {
        for (host, kind, _) in self.recorded_forge_hosts().await? {
            hosts::learn(&host, kind);
        }
        Ok(())
    }

    /// Ask `host` which forge it serves and record the answer.
    async fn ask_forge_type(&self, host: &str, scheme: &'static str) -> Result<ForgeKind> {
        let kind = hosts::probe(host, scheme, &self.user_account())
            .await
            .map_err(|error| {
                anyhow!(
                    "{error}; {} ({})",
                    forges::setting(host),
                    error.root_cause()
                )
            })?;
        self.record_forge_host(host, kind, scheme).await?;
        hosts::learn(host, kind);
        Ok(kind)
    }

    async fn recorded_forge_type(&self, host: &str) -> Result<Option<ForgeKind>> {
        let host = host.to_owned();
        self.store
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT kind FROM forge_hosts WHERE host=?1",
                        [host],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    async fn recorded_forge_hosts(&self) -> Result<Vec<Recorded>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT host,kind,scheme FROM forge_hosts ORDER BY host")?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    async fn record_forge_host(&self, host: &str, kind: ForgeKind, scheme: &str) -> Result<()> {
        let (host, scheme) = (host.to_owned(), scheme.to_owned());
        self.store
            .run(move |db| {
                db.execute(
                    "INSERT INTO forge_hosts(host,kind,scheme) VALUES (?1,?2,?3)
                        ON CONFLICT(host) DO UPDATE SET kind=excluded.kind, scheme=excluded.scheme",
                    params![host, kind, scheme],
                )?;
                Ok(())
            })
            .await
    }

    /// Ask every recorded host again, recording each new answer, and return
    /// the hosts whose forge changed; a pinned host is not asked.
    pub(crate) async fn recheck_forge_hosts(&self) -> Result<Vec<(String, Change)>> {
        let recorded = self.recorded_forge_hosts().await?;
        let rechecked = futures_util::future::join_all(recorded.into_iter().map(
            |(host, kind, scheme)| async move {
                let change = self.recheck_forge_host(&host, kind, &scheme).await;
                change.map(|change| (host, change))
            },
        ))
        .await;
        Ok(rechecked.into_iter().flatten().collect())
    }

    /// A host Shoal cannot reach keeps the forge it answered before.
    async fn recheck_forge_host(
        &self,
        host: &str,
        recorded: ForgeKind,
        scheme: &str,
    ) -> Option<Change> {
        if self.config().forges.pinned(host).is_some() {
            return None;
        }
        let scheme = if scheme == "http" { "http" } else { "https" };
        let _gate = self.gates.lock_forge_host(host).await;
        match self.ask_forge_type(host, scheme).await {
            Ok(kind) if kind != recorded => Some(Change {
                from: recorded,
                to: kind,
            }),
            Ok(_) => None,
            Err(error) => {
                log!("{host} not asked again; keeping {recorded}: {error:#}");
                None
            }
        }
    }
}

/// Make `pins` the forge types this process parses links of their hosts
/// as when they differ from `previous`, the pins it had; a host whose pin
/// went is parsed as what it was learned to serve.
pub(super) fn learn_forge_pins(previous: &Forges, pins: &Forges) {
    if pins != previous {
        hosts::pin(pins.pins());
    }
}

#[cfg(test)]
mod tests;
