//! What each forge does its own way with reactions on items and comments.
use anyhow::Result;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    items::{
        link::ItemKind,
        reaction::{Listed, Subject, allowed::AllowedReactions},
    },
    transport::api::Request,
};

/// How the forge lists, adds and removes reactions.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Reactions: Backend {
    /// The endpoint that lists the reactions on `subject` of item `number`.
    fn reactions_endpoint(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<String>;

    /// Whether the forge lists the reactions on `subject` in pages; by
    /// default it does.
    fn pages_reactions(&self, _subject: Subject) -> bool {
        true
    }

    /// A reaction as the forge lists it, with GitHub's name for its content.
    fn listed_reaction(&self, reaction: &Value) -> Result<Listed>;

    /// The content [`Self::listed_reaction`] reports for a reaction the
    /// forge names `name`; by default the name itself.
    fn reaction_content(&self, name: &str) -> String {
        name.to_owned()
    }

    /// The request that adds a reaction of `content` to the reactions at
    /// `endpoint`.
    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request>;

    /// The request that removes `own`, the account's reaction, from the
    /// reactions at `endpoint`.
    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request>;

    /// The reactions the forge allows; by default GitHub's, and only those.
    async fn allowed_reactions(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
    ) -> Result<AllowedReactions> {
        Ok(AllowedReactions::github(false))
    }
}
