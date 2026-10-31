//! Where GitLab's API serves a file uploaded to a project or group, such as
//! an image in a description: its web routes take no access token, while the
//! API serves the same file to a member.
use anyhow::{Context, Result};

use crate::transport::api::encode;

/// The API endpoint for the upload at `url`, a web URL naming its project or
/// group by ID, `/-/project/<id>/uploads/<secret>/<file>`, or by path,
/// `/<project path>/uploads/<secret>/<file>` and
/// `/groups/<group path>/-/uploads/<secret>/<file>`.
pub(crate) fn api_route(url: &str) -> Result<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    route(&segments).context(
        "GitLab downloads only files uploaded to a project or group, such as \
         /-/project/<id>/uploads/<secret>/<file> or /<project path>/uploads/<secret>/<file>",
    )
}

/// The API endpoint for the upload a web URL's path `segments` name.
fn route(segments: &[&str]) -> Option<String> {
    let [owner @ .., "uploads", secret, file] = segments else {
        return None;
    };
    let secret_valid =
        !secret.is_empty() && secret.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if !secret_valid || !is_name(file) {
        return None;
    }
    Some(format!(
        "{}/uploads/{secret}/{file}",
        owner_endpoint(owner)?
    ))
}

/// The API endpoint of the project or group the path `segments` before
/// `uploads` name.
fn owner_endpoint(segments: &[&str]) -> Option<String> {
    match segments {
        ["-", "project", id] => numeric(id).then(|| format!("projects/{id}")),
        ["-", "group", id] => numeric(id).then(|| format!("groups/{id}")),
        ["groups", group @ .., "-"] if is_path(group) => {
            Some(format!("groups/{}", encode(&group.join("/"), false)))
        }
        [_, _, ..] if is_path(segments) => {
            Some(format!("projects/{}", encode(&segments.join("/"), false)))
        }
        _ => None,
    }
}

fn numeric(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

/// Whether `segments` name a namespace path, without GitLab's `-` separator.
fn is_path(segments: &[&str]) -> bool {
    !segments.is_empty()
        && segments
            .iter()
            .all(|segment| is_name(segment) && *segment != "-")
}

/// Whether `segment` names something rather than moving along the path.
fn is_name(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
}

#[cfg(test)]
mod tests;
