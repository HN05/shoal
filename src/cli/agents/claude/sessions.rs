//! Claude Code's project folders: `<config>/projects/<directory>` holds the
//! sessions that ran in that directory, named after its path with every
//! character but ASCII letters and digits replaced by `-`.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::{cli::context::Context, env, execution::recovery};

/// The directory holding every project folder.
pub(super) fn projects(ctx: &Context) -> Result<PathBuf> {
    Ok(env::claude_config_dir()?
        .unwrap_or_else(|| ctx.paths.home.join(".claude"))
        .join("projects"))
}

/// The project folder holding `session`; `None` when no folder does.
pub(super) fn find(projects: &Path, session: &str) -> Result<Option<PathBuf>> {
    if !recovery::valid_session(session) {
        return Ok(None);
    }
    let file = format!("{session}.jsonl");
    let folders = match std::fs::read_dir(projects) {
        Ok(folders) => folders,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", projects.display())),
    };
    for folder in folders {
        let folder = folder?.path();
        if folder.join(&file).is_file() {
            return Ok(Some(folder));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn sessions_are_found_by_a_valid_id_in_any_project_folder() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        assert_eq!(find(&projects, "s1").unwrap(), None);
        fs::create_dir_all(projects.join("folder")).unwrap();
        fs::write(root.path().join("escape.jsonl"), "").unwrap();
        assert_eq!(find(&projects, "../../escape").unwrap(), None);
        fs::write(projects.join("folder/s1.jsonl"), "").unwrap();
        assert_eq!(
            find(&projects, "s1").unwrap(),
            Some(projects.join("folder"))
        );
    }
}
