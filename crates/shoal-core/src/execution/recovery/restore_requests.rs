//! Requests for the daemon to queue the restore of a stopped agent.
use crate::paths::StateFiles;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;

use super::{REQUEST_SUFFIX, consume};
use crate::{
    fsutil::atomic::{Permissions, ReplaceOptions},
    paths::Paths,
};

/// Ask the daemon to queue the restore of the agent stopped as `id`, which it
/// starts with `env`, the variables of the agent's terminal host, once the
/// execution has ended. The request is its own file beside the record, so
/// forgetting it never replaces a record a resume may hold.
pub fn request_restore(
    paths: &Paths,
    space_id: &SpaceId,
    id: &ExecutionId,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    std::fs::create_dir_all(paths.space_state(space_id))?;
    crate::fsutil::atomic::replace_atomically(
        &request_path(paths, space_id, id),
        &serde_json::to_vec(env)?,
        ReplaceOptions {
            permissions: Permissions::Temporary,
            sync: true,
        },
    )?;
    Ok(())
}

/// The space's restore requests: each stopped agent's execution ID with
/// the variables its restore starts with. An unreadable request asks for
/// nothing.
pub fn restore_requests(
    paths: &Paths,
    space_id: &SpaceId,
) -> Result<Vec<(ExecutionId, BTreeMap<String, String>)>> {
    let entries = match std::fs::read_dir(paths.space_state(space_id)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut requests = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(REQUEST_SUFFIX))
        else {
            continue;
        };
        let env = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if let Some(env) = env {
            requests.push((ExecutionId::from(id.to_owned()), env));
        }
    }
    requests.sort();
    Ok(requests)
}

pub fn forget_restore_request(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> Result<()> {
    consume(&request_path(paths, space_id, id))
}

fn request_path(paths: &Paths, space_id: &SpaceId, id: &ExecutionId) -> PathBuf {
    paths
        .space_state(space_id)
        .join(format!("{id}{REQUEST_SUFFIX}"))
}
