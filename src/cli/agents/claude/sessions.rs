//! Claude Code's project folders: `<config>/projects/<directory>` holds the
//! sessions that ran in that directory, named after its path with every
//! character but ASCII letters and digits replaced by `-`.
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, Result, ensure};

use crate::{cli::agents::AgentEnvironment, env, execution::recovery};

/// The longest folder name Claude Code uses as is; it shortens longer ones.
const MAX_FOLDER_NAME: usize = 200;

/// The directory holding every project folder of Claude Code running in
/// `environment`.
pub(super) fn projects(environment: &AgentEnvironment) -> Result<PathBuf> {
    Ok(env::claude_config_dir_with(environment.configured)?
        .unwrap_or_else(|| environment.home.join(".claude"))
        .join("projects"))
}

/// The project folder holding the latest copy of `session`, the one
/// modified last; `None` when no folder holds it.
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
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for folder in folders {
        let folder = folder?.path();
        let Ok(metadata) = std::fs::metadata(folder.join(&file)) else {
            continue;
        };
        let modified = metadata.modified()?;
        if metadata.is_file() && latest.as_ref().is_none_or(|(time, _)| modified > *time) {
            latest = Some((modified, folder));
        }
    }
    Ok(latest.map(|(_, folder)| folder))
}

/// Copy the project folder holding the latest copy of `session` into
/// `directory`'s, replacing only older files, so Claude Code resumes the
/// latest conversation there.
pub(super) fn carry(projects: &Path, session: &str, directory: &Path) -> Result<()> {
    let source = find(projects, session)?
        .with_context(|| format!("no project folder in {} holds it", projects.display()))?;
    let target = projects.join(folder_name(directory)?);
    if source == target {
        return Ok(());
    }
    crate::fsutil::copy_newer(&source, &target)
        .with_context(|| format!("copy {} to {}", source.display(), target.display()))
}

/// The project folder name of `directory`, which Claude Code derives from
/// the resolved path, one `-` per UTF-16 unit replaced.
fn folder_name(directory: &Path) -> Result<String> {
    let directory = std::fs::canonicalize(directory)
        .with_context(|| format!("resolve {}", directory.display()))?;
    let path = directory
        .to_str()
        .with_context(|| format!("{} is not UTF-8", directory.display()))?;
    let name: String = path
        .chars()
        .flat_map(|char| match char.is_ascii_alphanumeric() {
            true => vec![char],
            false => vec!['-'; char.len_utf16()],
        })
        .collect();
    ensure!(
        name.len() <= MAX_FOLDER_NAME,
        "Claude Code shortens the project folder name of {}, which is longer than {MAX_FOLDER_NAME} characters",
        directory.display()
    );
    Ok(name)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn folder_names_replace_each_utf16_unit_but_ascii_letters_and_digits() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("ws.2-ø😀");
        fs::create_dir(&directory).unwrap();
        let resolved = fs::canonicalize(root.path()).unwrap();
        let expected = format!(
            "{}-ws-2----",
            resolved.to_str().unwrap().replace(['/', '.', '_'], "-")
        );
        assert_eq!(folder_name(&directory).unwrap(), expected);
        let long = root.path().join("a".repeat(MAX_FOLDER_NAME));
        fs::create_dir(&long).unwrap();
        assert!(folder_name(&long).is_err());
    }

    /// Write `contents` to `path`, modified `seconds` after the epoch.
    fn write(path: &Path, contents: &str, seconds: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn carrying_a_session_copies_the_newer_files_of_its_latest_project_folder() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let folder = |name: &str| {
            let directory = root.path().join(name);
            fs::create_dir(&directory).unwrap();
            (projects.join(folder_name(&directory).unwrap()), directory)
        };
        let (old, old_directory) = folder("old");
        let (new, new_directory) = folder("new");
        write(&old.join("s1.jsonl"), "conversation\n", 20);
        write(&old.join("s1/subagents/agent.jsonl"), "subagent\n", 20);
        write(&old.join("other.jsonl"), "old\n", 10);
        // The new path held the session before, and has a newer other one.
        write(&new.join("s1.jsonl"), "stale\n", 5);
        write(&new.join("other.jsonl"), "new\n", 30);

        assert_eq!(find(&projects, "s1").unwrap(), Some(old.clone()));
        carry(&projects, "s1", &new_directory).unwrap();
        let read = |path: PathBuf| fs::read_to_string(path).unwrap();
        assert_eq!(read(new.join("s1.jsonl")), "conversation\n");
        assert_eq!(read(new.join("s1/subagents/agent.jsonl")), "subagent\n");
        assert_eq!(read(new.join("other.jsonl")), "new\n");
        assert_eq!(read(old.join("s1.jsonl")), "conversation\n");

        // The conversation continues at the new path, then moves back.
        write(&new.join("s1.jsonl"), "continued\n", 40);
        assert_eq!(find(&projects, "s1").unwrap(), Some(new.clone()));
        carry(&projects, "s1", &old_directory).unwrap();
        assert_eq!(read(old.join("s1.jsonl")), "continued\n");
        assert_eq!(read(old.join("other.jsonl")), "new\n");
        // Carrying where the latest copy is changes nothing.
        carry(&projects, "s1", &old_directory).unwrap();
        assert_eq!(read(old.join("s1.jsonl")), "continued\n");
    }

    #[test]
    fn sessions_are_found_by_a_valid_id_in_any_project_folder() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        assert_eq!(find(&projects, "s1").unwrap(), None);
        assert!(carry(&projects, "s1", root.path()).is_err());
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
