//! Claude Code keeps each session in the directory of the project it ran in.
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{AgentEnvironment, SessionFiles, Store, files_under};

pub(super) struct Claude;

impl Store for Claude {
    /// Its config directory's `projects` holds a directory per working
    /// directory.
    fn session_dir(&self, environment: &AgentEnvironment) -> Result<PathBuf> {
        let config = crate::client::env::claude_config_dir_from(|name| environment.variable(name))?
            .unwrap_or_else(|| environment.home.join(".claude"));
        Ok(config.join("projects"))
    }

    /// The transcript `<id>.jsonl` in a project's directory, and the
    /// directory `<id>` beside it with its subagents' transcripts and large
    /// tool results.
    fn session_files(&self, directory: &Path, id: &str) -> std::io::Result<SessionFiles> {
        let mut found = SessionFiles::default();
        for project in files_under(directory, directory, false)? {
            let project = directory.join(project);
            let transcript = project.join(format!("{id}.jsonl"));
            if transcript.is_file()
                && let Ok(relative) = transcript.strip_prefix(directory)
            {
                found.transcript.get_or_insert_with(|| relative.to_owned());
                found.files.push(relative.to_owned());
            }
            found
                .files
                .extend(files_under(directory, &project.join(id), true)?);
        }
        Ok(found)
    }
}
