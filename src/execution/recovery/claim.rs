//! Exclusive claims on saved records, so only one `shoal resume` restores,
//! reports or discards each.
use super::{Saved, consume};
use anyhow::{Context as _, Result, ensure};

pub(crate) struct RecoveryClaim(std::fs::File);

impl Drop for RecoveryClaim {
    fn drop(&mut self) {
        // Unlock before closing so a descriptor inherited by a forked child
        // cannot keep the recovery record claimed after this process drops it.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub(crate) fn claim(path: &std::path::Path) -> Result<RecoveryClaim> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("this recovery record is already being resumed")?;
    ensure!(path.try_exists()?, "recovery record was already consumed");
    Ok(RecoveryClaim(file))
}

/// Forget saved records, returning their execution IDs.
pub(crate) fn discard_saved(saved: &Saved, active: impl Fn(&str) -> bool) -> Result<Vec<String>> {
    let mut claims = Vec::new();
    for (id, path) in &saved.agents {
        ensure!(
            !active(id),
            "stopped agent {id} is active or unknown; stop or reconcile it first"
        );
        claims.push(claim(path)?);
    }
    for command in &saved.commands {
        claims.push(claim(&command.path)?);
    }
    for (_, path) in &saved.agents {
        std::fs::remove_file(path)?;
    }
    for command in &saved.commands {
        consume(&command.path)?;
    }
    Ok(saved
        .agents
        .iter()
        .map(|(id, _)| id.clone())
        .chain(saved.commands.iter().map(|command| command.id.clone()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::recovery::SavedCommand;

    #[test]
    fn a_recovery_record_cannot_be_claimed_twice() {
        let root = tempfile::tempdir().unwrap();
        let record = root.path().join("recovery.json");
        std::fs::write(&record, "{}").unwrap();
        let first = claim(&record).unwrap();
        assert!(claim(&record).is_err());
        std::fs::remove_file(&record).unwrap();
        drop(first);
        assert!(claim(&record).is_err());
    }

    #[test]
    fn discard_leaves_command_records_another_resume_is_reporting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("execution.command.json");
        std::fs::write(&path, r#"{"argv":["true"]}"#).unwrap();
        let saved = Saved {
            agents: vec![],
            commands: vec![SavedCommand {
                id: "execution".into(),
                path: path.clone(),
                argv: vec!["true".into()],
            }],
        };
        let reporting = claim(&path).unwrap();
        // Model a concurrent fork retaining the open file description until exec.
        let _inherited = reporting.0.try_clone().unwrap();
        assert!(discard_saved(&saved, |_| false).is_err());
        assert!(path.exists());
        drop(reporting);
        assert_eq!(discard_saved(&saved, |_| false).unwrap(), ["execution"]);
        assert!(!path.exists());
    }
}
