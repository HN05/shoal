//! Claude Code's project folders: `<config>/projects/<directory>` holds the
//! sessions that ran in that directory, named after its path with every
//! character but ASCII letters and digits replaced by `-`.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};

use crate::{cli::context::Context, env, execution::recovery};

/// The longest folder name Claude Code uses as is; it shortens longer ones.
const MAX_FOLDER_NAME: usize = 200;

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

/// Copy the project folder holding `session` into `directory`'s, keeping
/// what that folder already has, so Claude Code resumes it there.
pub(super) fn carry(projects: &Path, session: &str, directory: &Path) -> Result<()> {
    let source = find(projects, session)?
        .with_context(|| format!("no project folder in {} holds it", projects.display()))?;
    let target = projects.join(folder_name(directory)?);
    if source == target {
        return Ok(());
    }
    crate::fsutil::copy_missing(&source, &target)
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

    #[test]
    fn carrying_a_session_copies_its_project_folder_and_keeps_the_targets_files() {
        let root = tempfile::tempdir().unwrap();
        let projects = root.path().join("projects");
        let source = projects.join("-old-workspace");
        fs::create_dir_all(source.join("s1/subagents")).unwrap();
        fs::write(source.join("s1.jsonl"), "conversation\n").unwrap();
        fs::write(source.join("s1/subagents/agent.jsonl"), "subagent\n").unwrap();
        fs::write(source.join("other.jsonl"), "old\n").unwrap();
        let directory = root.path().join("new");
        fs::create_dir(&directory).unwrap();
        let target = projects.join(folder_name(&directory).unwrap());
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("other.jsonl"), "new\n").unwrap();

        assert_eq!(find(&projects, "s1").unwrap(), Some(source.clone()));
        carry(&projects, "s1", &directory).unwrap();
        let read = |path: PathBuf| fs::read_to_string(path).unwrap();
        assert_eq!(read(target.join("s1.jsonl")), "conversation\n");
        assert_eq!(read(target.join("s1/subagents/agent.jsonl")), "subagent\n");
        assert_eq!(read(target.join("other.jsonl")), "new\n");
        assert_eq!(read(source.join("s1.jsonl")), "conversation\n");
        // Carrying again finds the session in both folders and keeps them.
        carry(&projects, "s1", &directory).unwrap();
        assert_eq!(read(target.join("s1.jsonl")), "conversation\n");
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
