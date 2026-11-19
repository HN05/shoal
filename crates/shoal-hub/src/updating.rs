//! `shoal hub update`: the hub installs the latest release where it was
//! downloaded, from any of its machines; its daemon then restarts on it
//! once idle, and the machines that follow its version install it as the
//! hub refuses theirs. Homebrew updates its own installations.
use std::time::Duration;

use anyhow::Result;
use shoal_core::update::{self, HOMEBREW_UPGRADE, Installed, RELEASES, Target};
use shoal_protocol::hub::{HubUpdate, link::Call, link::Reply};

use super::answers::unexpected;
use crate::{
    Hub,
    daemon::Daemon,
    log,
    refusal::{Reason, Refusal},
};

/// How long a machine waits for the hub's update, which downloads and
/// unpacks a release.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

impl Hub {
    /// Install the latest release on the hub, on the hub itself or through
    /// this machine's link.
    pub async fn update_hub(&self, daemon: &dyn Daemon) -> Result<HubUpdate> {
        if self.is_hub(daemon) {
            return self.update_own_installation().await;
        }
        let calls = self.hub_calls().await?;
        match calls.call_within(Call::Update, UPDATE_TIMEOUT).await? {
            Reply::Updated(updated) => Ok(updated),
            reply => Err(unexpected(reply)),
        }
    }

    /// Install the latest release over the hub's own installation, refusing
    /// one that Homebrew installed or a development build.
    pub(super) async fn update_own_installation(&self) -> Result<HubUpdate> {
        let installation = match Installed::current()? {
            Installed::Homebrew => {
                return Err(Refusal::new(
                    Reason::NotReady,
                    format!("Homebrew installed Shoal on the hub; run {HOMEBREW_UPGRADE} there"),
                )
                .into());
            }
            _ if cfg!(debug_assertions) => {
                return Err(Refusal::new(
                    Reason::NotReady,
                    "the hub runs a development build, which never replaces itself",
                )
                .into());
            }
            Installed::Downloaded(installation) => installation,
        };
        let installed = update::update(RELEASES, &installation, &Target::Latest).await?;
        if let Some(version) = &installed {
            log!("hub: installed Shoal {version}; the daemon restarts on it once idle");
        }
        Ok(HubUpdate {
            previous: env!("CARGO_PKG_VERSION").to_owned(),
            installed,
        })
    }
}
