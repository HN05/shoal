//! What gate runs leave in a repository's directory: a run there holds back
//! the repository's move and removal, and a restarted daemon stops and
//! removes the runs a stopped one left.
use anyhow::Result;

use super::run::remove_run;
use crate::{
    Reviews,
    daemon::Daemon,
    log,
    shoal_dirs::{Contents, ShoalDir},
};
use shoal_ids::repository_ids::RepositoryId;

impl Reviews {
    /// Refuse while a gate run is in the repository `id`'s directory, as one
    /// whose agent may still work in its checkout, which a move or removal
    /// would take from under it.
    pub async fn ensure_no_gate_runs_in(
        &self,
        daemon: &dyn Daemon,
        id: &RepositoryId,
    ) -> Result<()> {
        let repository = daemon.repository(id).await?;
        let dir = ShoalDir::of(&repository, Contents::GateRuns);
        let running = tokio::task::spawn_blocking(move || -> Result<bool> {
            let entries = match dir.check_own()? {
                true => std::fs::read_dir(dir.root()).ok(),
                false => None,
            };
            let mut entries = entries.into_iter().flatten().flatten();
            Ok(entries.any(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir())))
        })
        .await??;
        anyhow::ensure!(
            !running,
            "the gate is deciding a request of the repository; wait for it, or restart the \
             daemon with `shoal daemon restart` when a stopped run left it"
        );
        Ok(())
    }

    /// Stop and remove the gate runs a stopped daemon left in each
    /// repository's directory; the requests they decided run again.
    pub async fn recover_gate_runs(&self, daemon: &dyn Daemon) -> Result<()> {
        for repository in daemon.repositories().await? {
            let dir = ShoalDir::of(&repository, Contents::GateRuns);
            if !dir.check_own()? {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(dir.root()) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let files = entry.path();
                match self.runs.stop_leftovers(&files).await {
                    Ok(()) => remove_run(&files),
                    Err(error) => {
                        log!("{}: {error:#}", files.display());
                        self.runs.track(&files);
                        daemon.hold_admission(&repository.id);
                    }
                }
            }
        }
        Ok(())
    }
}
