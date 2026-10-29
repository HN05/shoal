//! GitLab's award emoji as reactions, under GitHub's names for the ones
//! both forges have.
use anyhow::{Context, Result};
use serde_json::Value;

use super::username;
use crate::{
    ForgeRepo,
    account::Account,
    items::{
        item::text,
        link::ItemKind,
        reaction::{Listed, Subject},
    },
    transport::api::Request,
};

/// GitHub's reaction names and GitLab's emoji for them, where they differ.
const NAMES: [(&str, &str); 4] = [
    ("+1", "thumbsup"),
    ("-1", "thumbsdown"),
    ("laugh", "smile"),
    ("hooray", "tada"),
];

/// GitLab keeps a merge request's diff notes among its notes.
pub(super) fn endpoint(repo: &ForgeRepo, kind: ItemKind, number: u64, subject: Subject) -> String {
    let item = repo.item_endpoint(kind, number);
    match subject {
        Subject::Item => format!("{item}/award_emoji"),
        Subject::Comment(id) | Subject::ReviewComment(id) => {
            format!("{item}/notes/{id}/award_emoji")
        }
    }
}

pub(super) fn listed(award: &Value) -> Result<Listed> {
    let name = text(&award["name"]).context("an award emoji has no name")?;
    Ok(Listed {
        id: award["id"].clone(),
        content: github_name(name),
        login: username(&award["user"]),
    })
}

pub(super) async fn own_login(repo: &ForgeRepo, account: &Account) -> Result<String> {
    let user = repo.send(account, &Request::get("user".into())).await?;
    text(&user["username"]).context("GitLab did not report your username")
}

fn github_name(name: String) -> String {
    NAMES
        .iter()
        .find(|(_, emoji)| *emoji == name)
        .map_or(name, |(content, _)| (*content).to_owned())
}

#[cfg(test)]
mod tests;
