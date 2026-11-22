//! Any request to the forge's API, for reads no other operation covers.
//! Whether a caller may send one that changes something is the caller's
//! decision.
use anyhow::Result;
use serde_json::Value;

use crate::transport::{
    account::Account,
    api::{Request, TransportRequests, encode},
    backend::Backend,
};
use shoal_forge_types::ForgeRepo;
use shoal_forge_types::raw::HttpMethod;
use shoal_forge_types::raw::RawRequest;

/// Sending requests a caller wrote.
#[shoal_forge_derive::extension(pub RawRequests, pub(crate) RawSteps)]
impl ForgeRepo {
    /// The forge's JSON response to `raw`, sent as `account`; null for an
    /// empty one.
    pub async fn raw(&self, account: &Account, raw: &RawRequest) -> Result<Value> {
        let endpoint = self.raw_endpoint(raw)?;
        let endpoint = if raw.method == HttpMethod::Get {
            self.kind().raw_read(endpoint)
        } else {
            endpoint
        };
        let request = Request::new(raw.method, endpoint, raw.body.clone());
        self.send(account, &request).await
    }

    /// The path of `raw` relative to the API root, with `{owner}` and
    /// `{repo}` filled in from this repository.
    fn raw_endpoint(&self, raw: &RawRequest) -> Result<String> {
        raw.check()?;
        let (owner, repo) = self.path.rsplit_once('/').unwrap_or(("", &self.path));
        Ok(raw
            .path
            .trim_start_matches('/')
            .replace("{owner}", &encode(owner, false))
            .replace("{repo}", &encode(repo, false)))
    }
}

#[cfg(test)]
mod tests;
