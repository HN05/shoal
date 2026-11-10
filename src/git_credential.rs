//! The Git settings that give an agent launch's Git over HTTPS the agent
//! token of each host that has one, through Shoal's credential helper. For
//! each such host, an empty helper first clears the user's helpers for that
//! host only, so none of them answers for it or saves the token.
use anyhow::Result;

use crate::{
    paths::Paths,
    workers::{Worker, internal_command},
};

/// The settings for `hosts`, the hosts with an agent token, leaving out
/// those a credential helper in `profile`, the launch's Git profile
/// settings, covers.
pub fn settings<'a>(
    paths: &Paths,
    hosts: impl IntoIterator<Item = &'a str>,
    profile: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let hosts: Vec<_> = hosts
        .into_iter()
        .filter(|host| !profile.iter().any(|(key, _)| helper_covers(key, host)))
        .collect();
    if hosts.is_empty() {
        return Ok(Vec::new());
    }
    let helper = helper(paths)?;
    Ok(hosts
        .into_iter()
        .flat_map(|host| {
            let key = format!("credential.https://{host}.helper");
            [(key.clone(), String::new()), (key, helper.clone())]
        })
        .collect())
}

/// The helper setting's value: a shell command that runs this binary's
/// worker, to which Git appends the operation.
fn helper(paths: &Paths) -> Result<String> {
    let words = internal_command(paths, false, Worker::GitCredential)?
        .into_iter()
        .map(|word| {
            word.into_string()
                .map_err(|word| anyhow::anyhow!("{} is not UTF-8", word.to_string_lossy()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(format!("!{}", crate::shell::quote(&words)))
}

/// Whether the Git setting `key` is a credential helper Git asks for every
/// HTTPS request to `host`: `credential.helper`, or `credential.<url>.helper`
/// whose URL names that host, with or without `https://`. One whose URL also
/// names a user or a path answers only some requests, so it leaves the host
/// to Shoal's helper. Git compares the section and variable names without
/// case.
fn helper_covers(key: &str, host: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    let Some(rest) = lowered.strip_prefix("credential.") else {
        return false;
    };
    let Some(url) = rest.strip_suffix("helper") else {
        return false;
    };
    let Some(url) = url.strip_suffix('.') else {
        return url.is_empty();
    };
    let url = match url.split_once("://") {
        Some(("https", rest)) => rest,
        Some(_) => return false,
        None => url,
    };
    let (authority, path) = url.split_once('/').unwrap_or((url, ""));
    !authority.contains('@')
        && path.is_empty()
        && host_matches(authority, &host.to_ascii_lowercase())
}

/// Whether the host `pattern` matches `host`, where Git lets a `*` label
/// of a pattern match any one label of the host name and HTTPS's default
/// port matches a host without one.
fn host_matches(pattern: &str, host: &str) -> bool {
    let (pattern, pattern_port) = https_port(pattern);
    let (host, port) = https_port(host);
    let (patterns, labels): (Vec<_>, Vec<_>) =
        (pattern.split('.').collect(), host.split('.').collect());
    pattern_port == port
        && patterns.len() == labels.len()
        && patterns
            .iter()
            .zip(&labels)
            .all(|(pattern, label)| *pattern == "*" || pattern == label)
}

/// The host name and port of `authority`, with HTTPS's default port as
/// none.
fn https_port(authority: &str) -> (&str, &str) {
    match authority.split_once(':') {
        Some((host, "443")) | Some((host, "")) => (host, ""),
        Some((host, port)) => (host, port),
        None => (authority, ""),
    }
}

#[cfg(test)]
mod tests;
