//! Requests `shoal forge api` and `shoal forge download` send to the forge of
//! a space's repository or another one, so a scoped caller acts as its
//! agent account and only reads, unless its `api_writes` permission lets it
//! write to its own repository.
use anyhow::Result;
use serde_json::Value;
use shoal_daemon_base::{callers::Caller, permitted_actions::Action};
use shoal_forge::{download::DownloadRequests, raw::RawRequests, transport::account::Account};
use shoal_forge_types::{
    ForgeRepo,
    download::Download,
    raw::{HttpMethod, RawRequest},
};
use shoal_protocol::{config::permissions::PermissionKey, protocol::RepoTarget};

use crate::{Links, daemon::Daemon, place::Access};

impl Links {
    /// The forge's response to `request` in the `target` repository.
    pub async fn forge_api(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        request: &RawRequest,
        caller: Option<&Caller>,
    ) -> Result<Value> {
        let write = (request.method != HttpMethod::Get).then_some(request);
        let (forge, account) = self.api_forge(daemon, target, write, caller).await?;
        forge.raw(&account, request).await
    }

    /// The file at `url` on the forge of the `target` repository.
    pub async fn forge_download(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        url: &str,
        caller: Option<&Caller>,
    ) -> Result<Download> {
        let (forge, account) = self.api_forge(daemon, target, None, caller).await?;
        forge.download(&account, url).await
    }

    /// The `target` repository and the account that sends a request there,
    /// `write` when it is not a read: the agent account for a scoped
    /// `caller`, which writes only to its own repository as its
    /// `api_writes` permission allows, and the user's own logins otherwise.
    async fn api_forge(
        &self,
        daemon: &dyn Daemon,
        target: &RepoTarget,
        write: Option<&RawRequest>,
        caller: Option<&Caller>,
    ) -> Result<(ForgeRepo, Account)> {
        let access = match write {
            Some(_) => Access::Change,
            None => Access::Read,
        };
        let target = self
            .forge_target(daemon, target, None, access, caller)
            .await?;
        let Some(caller) = caller else {
            return Ok((target.into_forge()?, self.user_account()));
        };
        if let (Some(write), Some(place)) = (write, target.own_place()) {
            let repository = Some(place.repository_id().clone());
            let action = Action::request(PermissionKey::ApiWrites, write, repository);
            daemon.authorize_action(Some(caller), action).await?;
        }
        Ok((target.into_forge()?, self.agent_account()))
    }
}
