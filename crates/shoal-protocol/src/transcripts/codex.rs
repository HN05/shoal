//! Codex keeps each session in the directory of the day it started.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{AgentEnvironment, SessionFiles, Store, files_under};

pub(super) struct Codex;

impl Store for Codex {
    /// `sessions` in its home, with a directory per day.
    fn session_dir(&self, environment: &AgentEnvironment) -> Result<PathBuf> {
        let home = crate::env::codex_home_from(|name| environment.variable(name))?
            .unwrap_or_else(|| environment.home.join(".codex"));
        Ok(home.join("sessions"))
    }

    /// The transcript `rollout-<time>-<id>.jsonl` in the directory of the
    /// day it started.
    fn session_files(&self, directory: &Path, id: &str) -> std::io::Result<SessionFiles> {
        let suffix = format!("-{id}.jsonl");
        let files: Vec<_> = files_under(directory, directory, true)?
            .into_iter()
            .filter(|file| {
                file.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
            })
            .collect();
        Ok(SessionFiles {
            transcript: files.first().cloned(),
            files,
        })
    }
}
