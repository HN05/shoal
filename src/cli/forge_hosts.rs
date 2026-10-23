//! Which forge each host serves, as the CLI learns it: the type the global
//! config pins, a well-known name, else the daemon's answer, which it asks
//! the host for once.
use anyhow::Result;

use crate::{
    cli::client,
    config::forges::Forges,
    forge::{backends::ForgeKind, hosts::HostTypes},
    paths::Paths,
    protocol::Method,
};

pub struct ForgeHosts<'a> {
    pub(super) paths: &'a Paths,
    pub(super) forges: Forges,
}

impl HostTypes for ForgeHosts<'_> {
    async fn forge_type(&self, host: &str, scheme: &'static str) -> Result<ForgeKind> {
        if let Some(kind) = self.forges.settled(host) {
            return Ok(kind);
        }
        let method = Method::ForgeType {
            host: host.into(),
            scheme: scheme.into(),
        };
        client::request(self.paths, method).await
    }
}
