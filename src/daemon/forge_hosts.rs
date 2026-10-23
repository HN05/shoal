//! Which forge each host serves: the type the global config pins, a
//! well-known name, the answer the host gave when last asked, or else a new
//! answer, which is recorded so the host is asked once.
use std::future::Future;

use anyhow::{Result, anyhow, bail, ensure};
use rusqlite::{OptionalExtension, params};
use tokio::sync::OwnedMutexGuard;

use crate::{
    config::forges::{self, Forges},
    daemon::{
        doctor::{Check, CheckStatus},
        workspace::Manager,
    },
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
        let _gate = self.lock_forge_host(host).await;
        if let Some(kind) = self.recorded_forge_type(host).await? {
            return Ok(HostType::Answered(kind));
        }
        Ok(HostType::Answered(self.ask_forge_type(host, scheme).await?))
    }

    /// Hold `host`'s gate while asking it which forge it serves.
    async fn lock_forge_host(&self, host: &str) -> OwnedMutexGuard<()> {
        let gate = self
            .forge_host_gates
            .lock()
            .await
            .entry(host.to_owned())
            .or_default()
            .clone();
        gate.lock_owned().await
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

    /// Ask every recorded host again, recording each new answer; a pinned
    /// host is not asked.
    pub(crate) async fn forge_host_checks(&self) -> Result<Vec<Check>> {
        let recorded = self.recorded_forge_hosts().await?;
        Ok(futures_util::future::join_all(
            recorded
                .iter()
                .map(|(host, kind, scheme)| self.recheck_forge_host(host, *kind, scheme)),
        )
        .await)
    }

    async fn recheck_forge_host(&self, host: &str, recorded: ForgeKind, scheme: &str) -> Check {
        let name = forge_check_name(host);
        if let Some(kind) = self.config().forges.pinned(host) {
            return Check::new(
                name,
                CheckStatus::Ok,
                format!("Pinned to {kind} by the global config"),
            );
        }
        let scheme = if scheme == "http" { "http" } else { "https" };
        let _gate = self.lock_forge_host(host).await;
        match self.ask_forge_type(host, scheme).await {
            Ok(kind) if kind == recorded => {
                Check::new(name, CheckStatus::Ok, format!("Answers as {kind}"))
            }
            Ok(kind) => Check::new(
                name,
                CheckStatus::Warning,
                format!(
                    "Now answers as {kind} instead of {recorded}; Shoal uses {kind} from now on"
                ),
            ),
            Err(error) => Check::new(
                name,
                CheckStatus::Warning,
                format!("Could not ask it again, so Shoal keeps using {recorded}: {error:#}"),
            ),
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

pub fn forge_check_name(host: &str) -> String {
    format!("forge:{host}")
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use crate::{
        config::Config,
        forge::{ForgeRepo, backends::ForgeKind, hosts::HostType},
    };

    #[tokio::test]
    async fn a_lookup_waiting_on_the_host_takes_the_answer_the_first_one_records() {
        let (_root, manager) = crate::test_support::manager().await;
        let host = "waiting.invalid";
        let asking = manager.lock_forge_host(host).await;
        let gate = manager.forge_host_gates.lock().await[host].clone();
        let waiting = tokio::spawn({
            let manager = manager.clone();
            async move { manager.host_type(host, "https").await }
        });
        // The map, the asking lookup, this test and the waiting lookup.
        tokio::time::timeout(Duration::from_secs(60), async {
            while Arc::strong_count(&gate) < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the second lookup reaches the host's gate");
        manager
            .record_forge_host(host, ForgeKind::GitHub, "https")
            .await
            .unwrap();
        drop(asking);
        // Asking the host itself would fail: it does not resolve.
        assert_eq!(
            waiting.await.unwrap().unwrap(),
            HostType::Answered(ForgeKind::GitHub)
        );
    }

    #[tokio::test]
    async fn recorded_hosts_parse_as_their_forge_once_the_daemon_learns_them() {
        let (_root, manager) = crate::test_support::manager().await;
        let url = "https://ghe.recorded.example/team/repo/issues/7";
        manager
            .record_forge_host("ghe.recorded.example", ForgeKind::GitHub, "https")
            .await
            .unwrap();
        manager.learn_recorded_forge_hosts().await.unwrap();
        let repo = ForgeRepo::from_issue_url(url).unwrap();
        assert_eq!(
            repo.pull("8").unwrap().1,
            "https://ghe.recorded.example/team/repo/pull/8"
        );
    }

    #[tokio::test]
    async fn reloaded_pins_reach_link_parsing_and_removed_ones_leave_it() {
        let (_root, manager) = crate::test_support::manager().await;
        let url = "https://gitlab.reload.example/team/repo/issues/7";
        let config = Config {
            forges: toml::from_str("[\"gitlab.reload.example\"]\ntype = \"forgejo\"\n").unwrap(),
            ..Config::default()
        };
        manager.publish_config(config);
        assert!(ForgeRepo::from_issue_url(url).unwrap().issue(url).is_ok());
        // An answer a probe resolved before the reload does not replace it.
        crate::forge::hosts::learn("gitlab.reload.example", ForgeKind::GitLab);
        assert!(ForgeRepo::from_issue_url(url).unwrap().issue(url).is_ok());
        manager.publish_config(Config::default());
        // Without the pin, the host parses as what it was learned to serve.
        assert!(ForgeRepo::from_issue_url(url).unwrap().issue(url).is_err());
    }
}
