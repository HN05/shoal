//! An issue, PR or CI run, parsed once from the URL or number that names it
//! and passed on from there. Each serializes and stores as the text it was
//! parsed from, its canonical URL or the number.

use anyhow::Result;

use crate::{ItemRef, RepoRequests, hosts::HostTypes};
use shoal_forge_types::ForgeRepo;

/// Resolving an item's forge.
#[shoal_forge_derive::extension(pub ReferenceRequests)]
impl ItemRef {
    /// Its repository, on the forge `hosts` says its host serves.
    pub async fn forge(&self, hosts: &impl HostTypes) -> Result<ForgeRepo> {
        ForgeRepo::resolve(&self.repository().repository_url(), hosts).await
    }
}
