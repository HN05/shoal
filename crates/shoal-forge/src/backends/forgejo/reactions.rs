//! Forgejo's reactions on items and comments.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::Forgejo;
use crate::{
    ForgeRepo,
    account::Account,
    backends::{Reactions, github_rest},
    items::{
        link::ItemKind,
        reaction::{Listed, Subject, allowed::AllowedReactions},
    },
    transport::api::{HttpMethod, Request},
};

impl Reactions for Forgejo {
    fn reactions_endpoint(
        &self,
        repo: &ForgeRepo,
        _kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<String> {
        Ok(github_rest::reactions_endpoint(repo, number, subject, self))
    }

    /// Forgejo lists a comment's reactions all at once, ignoring pages.
    fn pages_reactions(&self, subject: Subject) -> bool {
        subject == Subject::Item
    }

    fn listed_reaction(&self, reaction: &Value) -> Result<Listed> {
        github_rest::listed_reaction(reaction)
    }

    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request> {
        Ok(github_rest::add_reaction(endpoint, content))
    }

    /// Forgejo removes the account's reaction by its content.
    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request> {
        Ok(Request::new(
            HttpMethod::Delete,
            endpoint,
            Some(json!({ "content": own.content })),
        ))
    }

    /// The instance's admin chooses them, under GitHub's names.
    async fn allowed_reactions(
        &self,
        repo: &ForgeRepo,
        account: &Account,
    ) -> Result<AllowedReactions> {
        let settings = repo
            .send(account, &Request::get("settings/ui".to_owned()))
            .await
            .context("read the forge's UI settings")?;
        AllowedReactions::only(&settings["allowed_reactions"])
    }
}
