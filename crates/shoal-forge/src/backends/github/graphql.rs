//! Reading the pages of a GitHub GraphQL connection.
use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::transport::{
    account::Account,
    api::{Request, TransportRequests},
};
use shoal_forge_types::{ForgeRepo, raw::HttpMethod};

/// Every node of the connection `connection` finds in the query's data,
/// following its pages from the `after` in `variables`.
pub(super) async fn graphql_nodes(
    repo: &ForgeRepo,
    account: &Account,
    query: &str,
    mut variables: Value,
    connection: fn(&Value) -> &Value,
) -> Result<Vec<Value>> {
    let mut nodes = Vec::new();
    loop {
        let request = Request::new(
            HttpMethod::Post,
            "graphql".into(),
            Some(json!({ "query": query, "variables": variables })),
        )
        .reading();
        let response = repo.send(account, &request).await?;
        let page = connection(&response["data"]);
        nodes.extend(
            page["nodes"]
                .as_array()
                .context("GitHub did not return a page of the list it was asked for")?
                .iter()
                .cloned(),
        );
        match next_cursor(page)? {
            Some(after) => variables["after"] = after.into(),
            None => return Ok(nodes),
        }
    }
}

/// The cursor of the page after `connection`'s, when there is one.
pub(super) fn next_cursor(connection: &Value) -> Result<Option<String>> {
    let info = &connection["pageInfo"];
    if info["hasNextPage"] != true {
        return Ok(None);
    }
    info["endCursor"]
        .as_str()
        .map(|cursor| Some(cursor.to_owned()))
        .context("GitHub did not return the next page's cursor")
}

#[cfg(test)]
mod tests;
