//! Which forge each host serves: the type the global config pins, a
//! well-known name, the answer the host gave when last asked, or else a new
//! answer, which is recorded so the host is asked once; and the
//! repositories that remotes, items and checkouts name on those forges.
use std::{future::Future, path::Path};

use anyhow::{Context, Result, anyhow};
use rusqlite::{OptionalExtension, params};
use shoal_core::{
    config::forges::{self, Forges},
    repository::remote_url_from_path,
};
use shoal_forge::{
    RepoRequests,
    ci::workflow::{WorkflowLink, WorkflowRequests},
    hosts::{self, HostType, HostTypes},
    items::reference::ReferenceRequests,
};
use shoal_protocol::model::Repository;

use super::ForgeAccess;
use crate::{
    forge::{ForgeRepo, backends::ForgeKind, items::reference::ItemRef},
    log,
};

/// The answers of [`ForgeAccess`], asking new hosts with the user's logins.
pub struct ForgeHosts<'a>(&'a ForgeAccess);

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
pub struct Change {
    pub(super) from: ForgeKind,
    pub(super) to: ForgeKind,
}

impl Change {
    /// What changed for `host`, for the user.
    pub fn message(&self, host: &str) -> String {
        let Self { from, to } = self;
        format!("{host} now answers as {to} instead of {from}; Shoal uses {to} from now on")
    }
}

impl ForgeAccess {
    /// The forge each host serves, as the forge crate's lookups ask it.
    pub fn hosts(&self) -> ForgeHosts<'_> {
        ForgeHosts(self)
    }

    /// The repository the remote or web URL `remote` names, on the forge
    /// its host serves.
    pub async fn forge(&self, remote: &str) -> Result<ForgeRepo> {
        ForgeRepo::resolve(remote, &self.hosts()).await
    }

    /// The forge the repository of `item` is on.
    pub async fn item_forge(&self, item: &ItemRef) -> Result<ForgeRepo> {
        item.forge(&self.hosts()).await
    }

    /// The repository `named` names, as OWNER/NAME on the forge of `own`
    /// or as a repository URL on any forge.
    pub async fn named_forge(&self, own: &ForgeRepo, named: &str) -> Result<ForgeRepo> {
        own.resolve_named(named, &self.hosts()).await
    }

    /// The repository the workflow URL `url` names, with the workflow and
    /// the branch it names.
    pub async fn workflow_forge(&self, url: &str) -> Result<(ForgeRepo, WorkflowLink)> {
        ForgeRepo::resolve_workflow_url(url, &self.hosts()).await
    }

    /// The forge of the `origin` of the checkout or worktree at `path`.
    pub async fn origin_forge(&self, path: &Path) -> Result<ForgeRepo> {
        let remote = remote_url_from_path(path)
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
    }

    /// The forge of the registered checkout's `origin`.
    pub async fn checkout_forge(&self, repository: &Repository) -> Result<ForgeRepo> {
        self.origin_forge(&repository.path).await
    }

    /// The forge `host` serves and what decided it, asking it over
    /// `scheme` when nothing says.
    pub async fn host_type(&self, host: &str, scheme: &'static str) -> Result<HostType> {
        if let Some(kind) = self.config.current().machine.forges.settled(host) {
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

    /// Remember the recorded answers for this process, as the daemon starts,
    /// so links of those hosts parse as their forges before a lookup.
    pub async fn learn_recorded_forge_hosts(&self) -> Result<()> {
        for (host, kind, _) in self.recorded_forge_hosts().await? {
            crate::forge::hosts::learn(&host, kind);
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
        crate::forge::hosts::learn(host, kind);
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

    pub(super) async fn record_forge_host(
        &self,
        host: &str,
        kind: ForgeKind,
        scheme: &str,
    ) -> Result<()> {
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
    pub async fn recheck_forge_hosts(&self) -> Result<Vec<(String, Change)>> {
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
        if self.config.current().machine.forges.pinned(host).is_some() {
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
pub fn learn_forge_pins(previous: &Forges, pins: &Forges) {
    if pins != previous {
        crate::forge::hosts::pin(pins.pins());
    }
}
