//! The Herdr workspace a start without a pane of its own opens its tab in:
//! the one `[terminal.herdr] workspace_name` names, found by label or created.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context as _, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::{
    sessions::{HostName, Origin, herdr},
    subprocess::Run,
};

/// The workspace labelled `label` on the server of `near`, or the one this
/// process reaches, created at `cwd` when there is none.
pub(super) async fn labelled(near: Option<&Origin>, label: &str, cwd: &Path) -> Result<Origin> {
    let data = near.map_or_else(server_here, |near| near.data.clone());
    let listed = list(herdr::command(&data)).await?;
    let workspace = match first_labelled(&listed, label) {
        Some(workspace) => workspace.to_owned(),
        None => create(herdr::command(&data), label, cwd).await?,
    };
    Ok(Origin {
        host: HostName::Herdr,
        group: Some(workspace),
        place: None,
        data,
        owned: false,
        instance: None,
    })
}

/// The variables of this process that choose a Herdr server and its
/// `herdr`; without them, `herdr` reaches its default server.
pub(super) fn server_here() -> BTreeMap<String, String> {
    herdr::SERVER_SELECTION
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), std::env::var(name).ok()?)))
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

/// The first of `workspaces` labelled `label`.
fn first_labelled<'a>(workspaces: &'a [Workspace], label: &str) -> Option<&'a str> {
    workspaces
        .iter()
        .find(|workspace| workspace.label.as_deref() == Some(label))
        .map(|workspace| workspace.workspace_id.as_str())
}

async fn list(mut herdr: Command) -> Result<Vec<Workspace>> {
    herdr.args(["workspace", "list"]);
    let output = Run::new(herdr)
        .checked()
        .await
        .context("list Herdr workspaces")?;
    let listed: Response<Listed> =
        serde_json::from_slice(&output.stdout).context("invalid Herdr workspace list")?;
    Ok(listed.result.workspaces)
}

/// A new workspace labelled `label` at `cwd`, left in the background.
async fn create(mut herdr: Command, label: &str, cwd: &Path) -> Result<String> {
    herdr
        .args(["workspace", "create", "--cwd"])
        .arg(cwd)
        .args(["--label", label, "--no-focus"]);
    let output = Run::new(herdr)
        .checked()
        .await
        .with_context(|| format!("create Herdr workspace {label}"))?;
    let created: Response<Created> =
        serde_json::from_slice(&output.stdout).context("invalid Herdr workspace creation")?;
    Ok(created.result.workspace.workspace_id)
}

#[derive(Deserialize)]
struct Response<T> {
    result: T,
}
#[derive(Deserialize)]
struct Listed {
    workspaces: Vec<Workspace>,
}
#[derive(Deserialize)]
struct Created {
    workspace: Workspace,
}
#[derive(Deserialize)]
struct Workspace {
    workspace_id: String,
    label: Option<String>,
}

#[cfg(test)]
mod tests;
