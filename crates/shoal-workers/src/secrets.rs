//! The secret tool worker.
use anyhow::{Context, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use crate::{client::connection::CommandPaths, env};
use shoal_core::secrets::{MANIFEST, Manifest, exported_names, fetch};
use shoal_protocol::{
    approvals::SecretSpecification,
    config::value::{Value, ValueKey},
    space_names::SpaceSelector,
};

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
    let values = fetch_served(paths, tool, served, &search).await?;
    exec_with(&program, args, values)
}

fn read_manifest(launchers: &Path) -> Result<Manifest> {
    let manifest = launchers.join(MANIFEST);
    serde_json::from_slice(
        &std::fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?,
    )
    .with_context(|| format!("parse {}", manifest.display()))
}

fn served_to<'a>(secrets: &'a Manifest, tool: &str) -> Vec<(&'a String, &'a SecretSpecification)> {
    secrets
        .iter()
        .filter(|(_, secret)| secret.tools.iter().any(|candidate| candidate == tool))
        .collect()
}

/// A scoped run asks the daemon about every secret, so current approval
/// policy applies even when it changed after the agent started; unscoped
/// callers need no approval.
async fn authorize(
    paths: &CommandPaths,
    tool: &str,
    served: &[(&String, &SecretSpecification)],
) -> Result<()> {
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

/// Fetch each served value. A command's value runs with PATH set to
/// `search`. A value written in the config, which the manifest holds only
/// as its digest, or one another machine prints, comes from the daemon.
async fn fetch_served<'a>(
    paths: &CommandPaths,
    tool: &str,
    served: Vec<(&'a String, &'a SecretSpecification)>,
    search: &OsStr,
) -> Result<Vec<(&'a String, OsString)>> {
    let mut values = Vec::new();
    for (name, source) in served {
        let value = match &source.value {
            Value::Written(_) | Value::Cmd { on: Some(_), .. } => {
                daemon_value(paths, tool, name, source).await?
            }
            value => fetch(name, value, Some(search)).await?,
        };
        values.push((name, value));
    }
    Ok(values)
}

/// The value of the secret `name`, which the daemon reads from the config
/// it still has as `source`.
async fn daemon_value(
    paths: &CommandPaths,
    tool: &str,
    name: &str,
    source: &SecretSpecification,
) -> Result<OsString> {
    let space = own_space(tool, name)?;
    let value = (secret_value(paths, space, name, source).await)
        .with_context(|| format!("{tool} needs secrets.{name}"))?;
    Ok(value.into())
}

/// The value the daemon reads for the secret `name` of `space`, which the
/// caller's launch resolved as `source`.
async fn secret_value(
    paths: &CommandPaths,
    space: SpaceSelector,
    name: &str,
    source: &SecretSpecification,
) -> Result<String> {
    let key = ValueKey::Secret {
        name: name.to_owned(),
        source: source.clone(),
    };
    crate::client::config::config_value(paths, space, key).await
}

/// The values of the environment secrets of `secrets` that another
/// machine prints, which the daemon reads for the launch in `space`.
pub async fn environment_elsewhere(
    paths: &CommandPaths,
    space: &SpaceSelector,
    secrets: &shoal_protocol::secrets::Secrets,
) -> Result<std::collections::BTreeMap<String, OsString>> {
    let mut values = std::collections::BTreeMap::new();
    for (name, secret) in shoal_core::secrets::environment_elsewhere(secrets) {
        let source = shoal_core::secrets::specification(secret);
        let value = secret_value(paths, space.clone(), name, &source)
            .await
            .with_context(|| format!("secrets.{name}"))?;
        values.insert(name.clone(), value.into());
    }
    Ok(values)
}

/// The space of the agent whose tool runs, which a launch names.
fn own_space(tool: &str, name: &str) -> Result<SpaceSelector> {
    let space = std::env::var(env::SPACE)
        .with_context(|| format!("{tool} needs secrets.{name}, but its space is unknown"))?;
    Ok(SpaceSelector::Id(SpaceId::from(space)))
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
    source: &SecretSpecification,
) -> Result<()> {
    use crate::protocol::{Body, SecretMethod};
    use shoal_protocol::{approvals::DecisionStatus, secrets::SecretRequest};
    let method = SecretMethod::SecretAcquire {
        workspace: own_space(tool, name)?,
        name: name.to_owned(),
        request: SecretRequest {
            reason: None,
            source: Some(source.clone()),
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
