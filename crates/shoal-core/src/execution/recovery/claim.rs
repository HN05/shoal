//! Exclusive claims on saved records, so only one `shoal resume` restores,
//! reports or forgets each.
use super::{
    LastSession, LastSessionExt as _, Record, RecordExt as _, Saved, SavedSession, consume,
};
use crate::client::execution_ids::ExecutionId;
use anyhow::{Context as _, Result, ensure};

pub struct RecoveryClaim(std::fs::File);

impl Drop for RecoveryClaim {
    fn drop(&mut self) {
        // Unlock before closing so a descriptor inherited by a forked child
        // cannot keep the recovery record claimed after this process drops it.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// How often a claim follows a record that was replaced while it locked it.
const CLAIM_ATTEMPTS: usize = 3;

pub fn claim(path: &std::path::Path) -> Result<RecoveryClaim> {
    for _ in 0..CLAIM_ATTEMPTS {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        if let Some(claim) = lock_current(file, path)? {
            return Ok(claim);
        }
    }
    anyhow::bail!("this recovery record is already being resumed")
}

/// Lock `file`, opened at `path`, unless another claim holds it. `None` when
/// `path` names another file by then: replacing a record, as clearing its
/// restore request does, leaves a lock on the old file guarding nothing.
fn lock_current(file: std::fs::File, path: &std::path::Path) -> Result<Option<RecoveryClaim>> {
    use std::os::unix::fs::MetadataExt as _;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("this recovery record is already being resumed")?;
    let current = match std::fs::metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("recovery record was already consumed")
        }
        Err(error) => return Err(error.into()),
    };
    let locked = file.metadata()?;
    let same = (locked.dev(), locked.ino()) == (current.dev(), current.ino());
    Ok(same.then_some(RecoveryClaim(file)))
}

/// Claim the stopped agent saved at `path` for a restore and read its
/// record as it is now, which a permission change may have rewritten since
/// it was listed.
pub fn claim_record(path: &std::path::Path) -> Result<(RecoveryClaim, Record)> {
    let claimed = claim(path)?;
    let record = Record::load(path)?.context("recovery record was already consumed")?;
    Ok((claimed, record))
}

/// Claim the last session saved at `path` for a restore and read it as it
/// is now, as [`claim_record`] does.
pub fn claim_last_session(path: &std::path::Path) -> Result<(RecoveryClaim, LastSession)> {
    let claimed = claim(path)?;
    let last = LastSession::load(path)?.context("last session was already continued")?;
    Ok((claimed, last))
}

/// Forget saved records, returning their execution IDs.
pub fn forget_saved(
    saved: &Saved,
    active: impl Fn(&ExecutionId) -> bool,
) -> Result<Vec<ExecutionId>> {
    let mut claims = Vec::new();
    for SavedSession { id, path, .. } in &saved.agents {
        ensure!(
            !active(id),
            "stopped agent {id} is active or unknown; stop or reconcile it first"
        );
        claims.push(claim(path)?);
    }
    for SavedSession { path, .. } in &saved.sessions {
        claims.push(claim(path)?);
    }
    for command in &saved.commands {
        claims.push(claim(&command.path)?);
    }
    for SavedSession { path, .. } in saved.agents.iter().chain(&saved.sessions) {
        std::fs::remove_file(path)?;
    }
    for command in &saved.commands {
        consume(&command.path)?;
    }
    Ok(saved.execution_ids())
}

#[cfg(test)]
mod tests;
