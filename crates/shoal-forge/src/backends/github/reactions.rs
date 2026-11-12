//! GitHub's reactions on items and comments.
use anyhow::Result;
use serde_json::Value;

use super::GitHub;
use crate::{
    ForgeRepo,
    backends::{Reactions, github_rest},
    items::{
        link::ItemKind,
        reaction::{Listed, Subject},
    },
    transport::api::{HttpMethod, Request},
};

impl Reactions for GitHub {
    fn reactions_endpoint(
        &self,
        repo: &ForgeRepo,
        _kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<String> {
        Ok(github_rest::reactions_endpoint(repo, number, subject, self))
    }

    fn listed_reaction(&self, reaction: &Value) -> Result<Listed> {
        github_rest::listed_reaction(reaction)
    }

    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request> {
        Ok(github_rest::add_reaction(endpoint, content))
    }

    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request> {
        let id = own.number()?;
        Ok(Request::new(
            HttpMethod::Delete,
            format!("{endpoint}/{id}"),
            None,
        ))
    }
}
