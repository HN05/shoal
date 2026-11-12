//! GitLab's award emoji as reactions, under GitHub's names for the ones
//! both forges have.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{GitLab, username};
use crate::{
    ForgeRepo,
    account::Account,
    backends::Reactions,
    items::{
        item::text,
        link::ItemKind,
        reaction::{Listed, Subject, allowed::AllowedReactions},
    },
    transport::api::{HttpMethod, Request},
};

impl Reactions for GitLab {
    fn reactions_endpoint(
        &self,
        repo: &ForgeRepo,
        kind: ItemKind,
        number: u64,
        subject: Subject,
    ) -> Result<String> {
        Ok(endpoint(repo, kind, number, subject))
    }

    fn listed_reaction(&self, reaction: &Value) -> Result<Listed> {
        listed(reaction)
    }

    fn reaction_content(&self, name: &str) -> String {
        github_name(name)
    }

    fn add_reaction(&self, endpoint: String, content: &str) -> Result<Request> {
        Ok(add(endpoint, content))
    }

    fn remove_reaction(&self, endpoint: String, own: &Listed) -> Result<Request> {
        remove(endpoint, own)
    }

    /// Any emoji GitLab knows, so GitHub's reactions are suggestions.
    async fn allowed_reactions(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
    ) -> Result<AllowedReactions> {
        Ok(AllowedReactions::github(true))
    }
}

/// GitHub's reaction names and GitLab's emoji for them, where they differ.
const NAMES: [(&str, &str); 4] = [
    ("+1", "thumbsup"),
    ("-1", "thumbsdown"),
    ("laugh", "smile"),
    ("hooray", "tada"),
];

/// GitLab keeps a merge request's diff notes among its notes.
fn endpoint(repo: &ForgeRepo, kind: ItemKind, number: u64, subject: Subject) -> String {
    let item = repo.item_endpoint(kind, number);
    match subject {
        Subject::Item => format!("{item}/award_emoji"),
        Subject::Comment(id) | Subject::ReviewComment(id) => {
            format!("{item}/notes/{id}/award_emoji")
        }
    }
}

fn listed(award: &Value) -> Result<Listed> {
    let name = text(&award["name"]).context("an award emoji has no name")?;
    Ok(Listed {
        id: award["id"].clone(),
        content: github_name(&name),
        login: username(&award["user"]),
    })
}

pub(super) async fn own_login(repo: &ForgeRepo, account: &Account) -> Result<String> {
    let user = repo.send(account, &Request::get("user".into())).await?;
    text(&user["username"]).context("GitLab did not report your username")
}

fn add(endpoint: String, content: &str) -> Request {
    let name = NAMES
        .iter()
        .find(|(github, _)| *github == content)
        .map_or(content, |(_, emoji)| emoji);
    Request::new(HttpMethod::Post, endpoint, Some(json!({ "name": name })))
}

fn remove(endpoint: String, own: &Listed) -> Result<Request> {
    let id = own.number()?;
    Ok(Request::new(
        HttpMethod::Delete,
        format!("{endpoint}/{id}"),
        None,
    ))
}

/// GitHub's name for GitLab's emoji `name`, where GitHub has the reaction.
fn github_name(name: &str) -> String {
    NAMES
        .iter()
        .find(|(_, emoji)| *emoji == name)
        .map_or(name, |(content, _)| content)
        .to_owned()
}

#[cfg(test)]
mod tests;
