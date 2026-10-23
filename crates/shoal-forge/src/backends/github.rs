//! GitHub, through `gh`, which owns the login: its CLI reports items and
//! activity as structured data, and `gh api` sends REST and GraphQL requests.
use anyhow::{Result, bail};
use serde_json::Value;

use super::Backend;
use crate::{
    ForgeRepo,
    account::Account,
    repo::locator::ItemRoute,
    transport::{
        api::{Request, full_page},
        programs,
    },
};

#[derive(Clone, Copy)]
pub(crate) struct GitHub;

impl Backend for GitHub {
    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pull
    }

    fn repo_endpoint(&self, repo: &ForgeRepo, path: &str) -> String {
        format!("repos/{}/{path}", repo.path)
    }

    fn pull_resource(&self) -> &'static str {
        "pulls"
    }

    fn number_field(&self) -> &'static str {
        "number"
    }

    fn commit_field(&self) -> &'static str {
        "sha"
    }

    fn login_hint(&self) -> &'static str {
        "check `gh auth login` and repository access"
    }

    async fn send(&self, repo: &ForgeRepo, account: &Account, request: &Request) -> Result<Value> {
        let body = repo.passthrough(programs::GH, account, request).await?;
        // GraphQL reports a failed mutation in a successful response.
        if let Some(errors) = graphql_errors(&body) {
            bail!("{errors}");
        }
        Ok(body)
    }

    fn page_size(&self) -> (&'static str, usize) {
        ("per_page", 100)
    }

    fn page<'a>(&self, response: &'a Value, size: usize) -> Option<(&'a [Value], bool)> {
        full_page(response, size)
    }
}

/// The messages of a GraphQL response's `errors`, when it has any.
fn graphql_errors(body: &Value) -> Option<String> {
    let errors = body["errors"]
        .as_array()
        .filter(|errors| !errors.is_empty())?;
    Some(
        errors
            .iter()
            .map(|error| error["message"].as_str().unwrap_or("GraphQL error"))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_errors_fail_a_successful_response() {
        let failed = serde_json::json!({"data": null, "errors": [{"message": "not allowed"}, {}]});
        assert_eq!(
            graphql_errors(&failed).unwrap(),
            "not allowed; GraphQL error"
        );
        assert_eq!(
            graphql_errors(&serde_json::json!({"data": {}, "errors": []})),
            None
        );
        assert_eq!(graphql_errors(&serde_json::json!([])), None);
    }
}
