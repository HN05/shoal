//! Following the hub's version. Once the hub refuses this machine for
//! running another version, with its own signed, a Shoal installed from a
//! release download with `[machine.hub] auto_update` on installs exactly
//! the hub's release, as `shoal update --version` does, a downgrade
//! included; the daemon then restarts on it once idle, keeping its agents
//! running, and connects again. Otherwise the user hears what to run, and
//! the machine works on its own until its version matches.
use std::{sync::Mutex, time::Duration};

use anyhow::Result;
use shoal_core::update::{self, HOMEBREW_UPGRADE, Installation, Installed, RELEASES, Target};
use tokio::time::Instant;

use shoal_daemon_base::notifications::About;
use shoal_protocol::notifications::NotificationKind;

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

/// The command that brings this machine to `version`.
pub(super) fn command(version: &str) -> String {
    match Installed::current() {
        Ok(Installed::Homebrew) => HOMEBREW_UPGRADE.to_owned(),
        _ => format!("shoal update --version {version}"),
    }
}

impl Hub {
    /// Follow the hub to its `version`, at most once per [`RETRY`].
    pub(super) async fn follow_hub(&self, daemon: &dyn Daemon, version: &str) {
        if !self.member.followed.take(version) {
            return;
        }
        let auto_update = self.hub_settings(daemon).is_some_and(|hub| hub.auto_update);
        let stays = "so this machine stays off the hub until it runs that version";
        let message = match Installed::current()
            .map(|installed| way(auto_update, cfg!(debug_assertions), installed))
        {
            Ok(Way::Install(installation)) => {
                match self.install_hub_version(&installation, version).await {
                    Ok(()) => return,
                    Err(error) => format!(
                        "Shoal could not install {version}, the hub's version, {stays}: {error:#}; run shoal update --version {version} here"
                    ),
                }
            }
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
        self.notify(
            daemon,
            About::named(None),
            NotificationKind::HubRefused,
            message,
        )
        .await;
    }

    /// Install the hub's `version` over `installation`.
    async fn install_hub_version(&self, installation: &Installation, version: &str) -> Result<()> {
        log!("hub: installing Shoal {version}, the hub's version");
        let target = Target::Exact(version.to_owned());
        update::update(RELEASES, installation, &target).await?;
        self.member.followed.installed(version);
        log!("hub: installed Shoal {version}; the daemon restarts on it once idle");
        Ok(())
    }
}

#[cfg(test)]
mod tests;
