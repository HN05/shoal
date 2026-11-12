//! Bitbucket's reactions on items and comments.
use anyhow::Result;
use serde_json::Value;

use super::Bitbucket;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Reactions,
    capability::{self, Capability},
    items::{
        link::ItemKind,
        reaction::{Listed, Subject, allowed::AllowedReactions},
    },
    transport::api::Request,
};

impl Reactions for Bitbucket {
    fn reactions_endpoint(
        &self,
        _repo: &ForgeRepo,
        _kind: ItemKind,
        _number: u64,
        _subject: Subject,
    ) -> Result<String> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn listed_reaction(&self, _reaction: &Value) -> Result<Listed> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn add_reaction(&self, _endpoint: String, _content: &str) -> Result<Request> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    fn remove_reaction(&self, _endpoint: String, _own: &Listed) -> Result<Request> {
        Err(capability::unsupported(self, Capability::Reactions))
    }

    async fn allowed_reactions(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
    ) -> Result<AllowedReactions> {
        Err(capability::unsupported(self, Capability::Reactions))
    }
}
