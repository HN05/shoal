//! The secret tool worker: it runs a tool with the secrets a launch's
//! launchers serve it, once the daemon authorizes each one.
use anyhow::{Context, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use crate::{client::connection::CommandPaths, env};
use shoal_core::secrets::{MANIFEST, Secret, exported_names, fetch, specification};
use shoal_protocol::{secrets::Secrets, space_names::SpaceSelector};

/// Run `tool` with the secrets configured for it in place of the launcher
/// in `launchers`: the next `tool` on PATH, which may be an enclosing
/// launch's launcher. Values are fetched only after the daemon authorizes
/// every one.
pub async fn run_tool(
    paths: &CommandPaths,
    launchers: &Path,
    tool: &str,
    args: Vec<OsString>,
) -> Result<i32> {
    let secrets = read_manifest(launchers)?;
    let served = served_to(&secrets, tool);
    let search = path_after(&std::env::var_os("PATH").unwrap_or_default(), launchers)?;
    let program = crate::fsutil::executables::find_executable(OsStr::new(tool), &search)
        .with_context(|| format!("{tool} is not on PATH"))?;
    authorize(paths, tool, &served).await?;
    let values = fetch_served(served, &search).await?;
    exec_with(&program, args, values)
}

fn read_manifest(launchers: &Path) -> Result<Secrets> {
    let manifest = launchers.join(MANIFEST);
    serde_json::from_slice(
        &std::fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?,
    )
    .with_context(|| format!("parse {}", manifest.display()))
}

fn served_to<'a>(secrets: &'a Secrets, tool: &str) -> Vec<(&'a String, &'a Secret)> {
    secrets
        .iter()
        .filter(|(_, secret)| secret.tools.iter().any(|candidate| candidate == tool))
        .collect()
}

/// A scoped run asks the daemon about every secret, so current approval
/// policy applies even when it changed after the agent started; unscoped
/// callers need no approval.
async fn authorize(paths: &CommandPaths, tool: &str, served: &[(&String, &Secret)]) -> Result<()> {
    if !must_authorize(
        tool,
        crate::process::scoped::inherits_scope(&paths.state),
        env::is_scoped(),
    )? {
        return Ok(());
    }
    for (name, secret) in served {
        confirm_grant(paths, tool, name, secret).await?;
    }
    Ok(())
}

/// Whether a run must ask the daemon. A run scoped by ancestry must still
/// carry its token: the daemon treats a request without one as unscoped.
fn must_authorize(tool: &str, inherited: bool, token: bool) -> Result<bool> {
    ensure!(
        !inherited || token,
        "{tool} cannot fetch secrets once {} is cleared",
        env::SCOPE_TOKEN
    );
    Ok(inherited)
}

/// Fetch each served value with PATH set to `search`.
async fn fetch_served<'a>(
    served: Vec<(&'a String, &'a Secret)>,
    search: &OsStr,
) -> Result<Vec<(&'a String, OsString)>> {
    let mut values = Vec::new();
    for (name, secret) in served {
        values.push((name, fetch(name, secret, Some(search)).await?));
    }
    Ok(values)
}

/// Replace this process with `program`, adding the fetched values.
fn exec_with(program: &Path, args: Vec<OsString>, values: Vec<(&String, OsString)>) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(program);
    command.args(args);
    command.env(
        env::SECRET_ENV,
        exported_names(values.iter().map(|(name, _)| *name)),
    );
    command.envs(values);
    Err(command.exec()).with_context(|| format!("run {}", program.display()))
}

/// Fail unless the daemon confirms this workspace may fetch `name` from the
/// source this launch serves; a pending or denied request is named so the
/// agent can follow it.
async fn confirm_grant(
    paths: &CommandPaths,
    tool: &str,
    name: &str,
    secret: &Secret,
) -> Result<()> {
    use crate::protocol::{Body, SecretMethod};
    use shoal_protocol::{approvals::DecisionStatus, secrets::SecretRequest};
    let workspace = std::env::var(env::SPACE)
        .with_context(|| format!("{tool} needs secrets.{name}, but its space is unknown"))?;
    let method = SecretMethod::SecretAcquire {
        workspace: SpaceSelector::Id(SpaceId::from(workspace)),
        name: name.to_owned(),
        request: SecretRequest {
            reason: None,
            source: Some(specification(secret)),
        },
    };
    let body = crate::client::connection::call(paths, method)
        .await
        .with_context(|| format!("{tool} needs secrets.{name}"))?;
    match body {
        Body::SecretGrant(_) => Ok(()),
        Body::ApprovalRequest(request) if request.status == DecisionStatus::Denied => bail!(
            "{tool} needs secrets.{name}: the user denied approval {}",
            request.number
        ),
        Body::ApprovalRequest(request) => bail!(
            "{tool} needs secrets.{name}: approval {} is pending; retry after the user decides",
            request.number
        ),
        body => Err(body.unexpected("SecretGrant or ApprovalRequest")),
    }
}

/// The PATH entries after `launchers`, or every other entry when PATH does
/// not hold it. An enclosing launch's launchers come later on PATH, so this
/// never leads back to an inner launcher for the same tool.
fn path_after(path: &OsStr, launchers: &Path) -> Result<OsString> {
    let entries: Vec<_> = std::env::split_paths(path).collect();
    let start = entries
        .iter()
        .position(|entry| env::same_directory(entry, launchers))
        .map_or(0, |index| index + 1);
    Ok(std::env::join_paths(
        entries[start..]
            .iter()
            .filter(|entry| !env::same_directory(entry, launchers)),
    )?)
}

#[cfg(test)]
mod tests;
