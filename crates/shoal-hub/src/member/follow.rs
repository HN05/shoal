//! Following the hub's version. Once the hub refuses this machine for
//! running another version, with its own signed, a Shoal installed from a
//! release download with `[machine.hub] auto_update` on installs exactly
//! the hub's release, as `shoal update --version` does, a downgrade
//! included; a managed daemon then restarts on it once idle, keeping its
//! agents running, and connects again, while a foreground one tells the user
//! to restart it. Otherwise the user hears what to run, and
//! the machine works on its own until its version matches.
use std::{sync::Mutex, time::Duration};

use anyhow::Result;
use shoal_core::update::{self, HOMEBREW_UPGRADE, Installation, Installed, RELEASES, Target};
use shoal_protocol::hub::link::Updates;
use tokio::time::Instant;

use shoal_daemon_base::notifications::About;
use shoal_protocol::alerts::AlertKind;

use crate::{Hub, daemon::Daemon, log};

/// How long this machine waits before acting again on a version it did not
/// install.
const RETRY: Duration = Duration::from_secs(60 * 60);

/// The hub's version this machine last acted on.
#[derive(Default)]
pub(super) struct Followed(Mutex<Option<Last>>);

struct Last {
    version: String,
    at: Instant,
    installed: bool,
}

impl Followed {
    /// Whether to act on `version` now, which it then is not for
    /// [`RETRY`], nor ever again once installed.
    fn take(&self, version: &str) -> bool {
        let mut last = self.0.lock().unwrap();
        let due = match &*last {
            Some(last) if last.version == version => !last.installed && last.at.elapsed() >= RETRY,
            _ => true,
        };
        if due {
            *last = Some(Last {
                version: version.to_owned(),
                at: Instant::now(),
                installed: false,
            });
        }
        due
    }

    /// Record that `version` is installed.
    fn installed(&self, version: &str) {
        if let Some(last) = &mut *self.0.lock().unwrap()
            && last.version == version
        {
            last.installed = true;
        }
    }
}

/// How this machine follows the hub to another version.
#[derive(Debug, PartialEq)]
enum Way {
    /// Shoal installs it over this installation.
    Install(Installation),
    /// The user does, with Homebrew.
    Homebrew,
    /// The user does, with `shoal update --version`, for `why`.
    Manual { why: &'static str },
}

/// How an `installed` Shoal follows the hub, with `[machine.hub]
/// auto_update` as `auto_update` sets it, from a `development` build or a
/// release.
fn way(auto_update: bool, development: bool, installed: Installed) -> Way {
    match installed {
        Installed::Homebrew => Way::Homebrew,
        Installed::Downloaded(_) if development => Way::Manual {
            why: "this is a development build",
        },
        Installed::Downloaded(_) if !auto_update => Way::Manual {
            why: "[machine.hub] auto_update is off here",
        },
        Installed::Downloaded(installation) => Way::Install(installation),
    }
}

/// Whether a release whose newest state schema is `schema` reads the state
/// this machine's daemon keeps: an older one cannot start on state a newer
/// release migrated past it.
pub(super) fn reads_state(schema: i64) -> bool {
    schema >= shoal_store::SCHEMA_VERSION
}

/// Whether Homebrew can bring this machine to `version`: it only ever
/// upgrades, so never to a release older than the one running here.
fn homebrew_reaches(version: &str) -> bool {
    !update::is_newer(env!("CARGO_PKG_VERSION"), version).unwrap_or(false)
}

/// What brings this machine to `version`, as a join refused for another
/// version tells the user.
pub(super) fn join_advice(version: &str) -> String {
    match Installed::current() {
        Ok(Installed::Homebrew) if !homebrew_reaches(version) => {
            "update the hub, then join again".to_owned()
        }
        Ok(Installed::Homebrew) => format!("run {HOMEBREW_UPGRADE} here, then join again"),
        _ => format!("run shoal update --version {version} here, then join again"),
    }
}

impl Way {
    /// How the hub's alert about this machine names its update.
    fn updates(&self) -> Updates {
        match self {
            Self::Install(_) => Updates::Follows,
            Self::Homebrew => Updates::Homebrew,
            Self::Manual { .. } => Updates::Manual,
        }
    }
}

impl Hub {
    /// How this machine follows the hub's version.
    fn way(&self, daemon: &dyn Daemon) -> Result<Way> {
        let auto_update = self.hub_settings(daemon).is_some_and(|hub| hub.auto_update);
        let installed = Installed::current()?;
        Ok(way(auto_update, cfg!(debug_assertions), installed))
    }

    /// How this machine comes to run the hub's version, as its hello
    /// states it.
    pub(super) fn updates(&self, daemon: &dyn Daemon) -> Updates {
        self.way(daemon)
            .map_or(Updates::Manual, |way| way.updates())
    }

    /// Follow the hub to its `version`, whose newest state schema is
    /// `schema`, at most once per [`RETRY`].
    pub(super) async fn follow_hub(&self, daemon: &dyn Daemon, version: &str, schema: i64) {
        if !self.member.followed.take(version) {
            return;
        }
        let stays = "so this machine stays off the hub until it runs that version";
        let message = match self.way(daemon) {
            _ if !reads_state(schema) => format!(
                "The hub runs Shoal {version}, older than this machine's {}, which cannot read the state this machine keeps, {stays}: run shoal update on the hub, or shoal hub update from a machine connected to it",
                env!("CARGO_PKG_VERSION")
            ),
            Ok(Way::Install(installation)) => {
                match self.install_hub_version(&installation, version).await {
                    Ok(()) if self.member.restarts() => return,
                    Ok(()) => format!(
                        "Shoal installed {version}, the hub's version, but this daemon runs in the foreground, {stays}: restart it"
                    ),
                    Err(error) => format!(
                        "Shoal could not install {version}, the hub's version, {stays}: {error:#}; run shoal update --version {version} here"
                    ),
                }
            }
            Ok(Way::Homebrew) if !homebrew_reaches(version) => format!(
                "The hub runs Shoal {version}, older than this machine's {}, which Homebrew never installs, {stays}: run shoal update on the hub, or shoal hub update from a machine connected to it",
                env!("CARGO_PKG_VERSION")
            ),
            Ok(Way::Homebrew) => format!(
                "The hub runs Shoal {version}, and Homebrew installed Shoal here, {stays}: run {HOMEBREW_UPGRADE}"
            ),
            Ok(Way::Manual { why }) => format!(
                "The hub runs Shoal {version}, and {why}, {stays}: run shoal update --version {version}"
            ),
            Err(error) => format!(
                "The hub runs Shoal {version}, {stays}, and Shoal could not tell how it was installed here: {error:#}"
            ),
        };
        log!("hub: {message}");
        self.notify(daemon, About::named(None), AlertKind::HubRefused, message)
            .await;
    }

    /// Install the hub's `version` over `installation`.
    async fn install_hub_version(&self, installation: &Installation, version: &str) -> Result<()> {
        log!("hub: installing Shoal {version}, the hub's version");
        let target = Target::Exact(version.to_owned());
        update::update(RELEASES, installation, &target).await?;
        self.member.followed.installed(version);
        log!("hub: installed Shoal {version}, the hub's version");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
