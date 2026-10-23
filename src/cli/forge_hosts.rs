//! Which forge each host serves, as the CLI learns it: the type the global
//! config pins, a well-known name, else the daemon's answer, which it asks
//! the host for once.
use anyhow::Result;

use crate::{
    cli::client,
    config::forges::Forges,
    forge::hosts::{HostType, HostTypes},
    paths::Paths,
    protocol::Method,
};

pub struct ForgeHosts<'a> {
    pub(super) paths: &'a Paths,
    pub(super) forges: Forges,
}

impl HostTypes for ForgeHosts<'_> {
    async fn forge_type(&self, host: &str, scheme: &'static str) -> Result<HostType> {
        if let Some(kind) = self.forges.settled(host) {
            return Ok(HostType::Settled(kind));
        }
        let method = Method::ForgeType {
            host: host.into(),
            scheme: scheme.into(),
        };
        // The daemon's own pins are this config's, which outrank its answer.
        Ok(HostType::Answered(
            client::request(self.paths, method).await?,
        ))
    }
}
