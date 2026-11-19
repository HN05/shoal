//! The Git settings that give an agent launch's Git over HTTPS the agent
//! token of each host that has one, through Shoal's credential helper. For
//! each such host, an empty helper first clears the user's helpers for that
//! host only, so none of them answers for it or saves the token, and Git
//! asks Shoal's helper before its first request, so libcurl never sends a
//! login of the user's `.netrc` for the host first, which Git 2.46 and newer
//! do.

use std::ffi::OsStr;

use anyhow::Result;

use crate::{
    paths::Paths,
    workers::{GIT_CREDENTIAL, GROUP, Worker, internal_command},
};

/// The settings for `hosts`, the hosts with an agent token, leaving out
/// those a credential helper in `profile`, the launch's Git profile
/// settings, covers.
pub(crate) fn settings<'a>(
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
            let context = format!("https://{host}");
            let key = helper_key(&context);
            [
                (key.clone(), String::new()),
                (key, helper.clone()),
                (proactive_key(&context), PROACTIVE.to_owned()),
            ]
        })
        .collect())
}

/// Remove Shoal's credential helpers, with the resets before them and the
/// proactive logins after, from the Git settings `command` passes on, for a
/// push that goes out as the user.
pub fn strip(command: &mut tokio::process::Command) {
    let contexts: Vec<String> = super::passed_on(command)
        .into_iter()
        .filter(|(_, value)| is_helper(value))
        .filter_map(|(key, _)| helper_context(&key.to_string_lossy()).map(str::to_owned))
        .collect();
    super::retain(command, |(key, value)| {
        let key = key.to_string_lossy();
        !contexts.iter().any(|context| {
            let helper = key == helper_key(context) && (value.is_empty() || is_helper(value));
            helper || (*key == *proactive_key(context) && value == PROACTIVE)
        })
    });
}

/// The value of `http.<url>.proactiveAuth` that has Git ask the credential
/// helper for a Basic login before its first request to the URL.
const PROACTIVE: &str = "basic";

/// The key of the credential helper for the URL `context`.
fn helper_key(context: &str) -> String {
    format!("credential.{context}.helper")
}

/// The key of the proactive login for the URL `context`.
fn proactive_key(context: &str) -> String {
    format!("http.{context}.proactiveAuth")
}

/// The URL context of [`helper_key`]'s `key`.
fn helper_context(key: &str) -> Option<&str> {
    key.strip_prefix("credential.")?.strip_suffix(".helper")
}

/// Whether the Git setting `value` is Shoal's helper.
fn is_helper(value: &OsStr) -> bool {
    let value = value.to_string_lossy();
    value.starts_with('!') && value.ends_with(&format!(" {GROUP} {GIT_CREDENTIAL}"))
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
/// HTTPS request to `host`: `credential.helper`, or `credential.<context>.helper`
/// whose context names that host. Git compares a URL context's scheme and
/// host without case, and reads a context without a scheme literally, as
/// a host to compare exactly. A context that also names a user or a path
/// answers only some requests, so it leaves the host to Shoal's helper.
/// Git compares the section and variable names without case.
fn helper_covers(key: &str, host: &str) -> bool {
    const SECTION: &str = "credential.";
    const VARIABLE: &str = ".helper";
    let lowered = key.to_ascii_lowercase();
    if lowered == "credential.helper" {
        return true;
    }
    if !lowered.starts_with(SECTION)
        || !lowered.ends_with(VARIABLE)
        || key.len() < SECTION.len() + VARIABLE.len()
    {
        return false;
    }
    let context = &key[SECTION.len()..key.len() - VARIABLE.len()];
    let Some((scheme, url)) = context.split_once("://") else {
        return context == host;
    };
    let (authority, path) = url.split_once('/').unwrap_or((url, ""));
    scheme.eq_ignore_ascii_case("https")
        && !authority.contains('@')
        && path.is_empty()
        && host_matches(&authority.to_ascii_lowercase(), &host.to_ascii_lowercase())
}

/// Whether the URL host `pattern` matches `host`, where Git lets a `*` label
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
