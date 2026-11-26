//! Where a review's run lives: its directory and files, held while it runs
//! and removed after.
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use shoal_ids::repository_ids::RepositoryId;

use crate::{
    Reviews,
    checkouts::CheckoutHold,
    daemon::Admission,
    git, log,
    model::Repository,
    reviewer::Files,
    reviews::{Review, ReviewId, ReviewTarget},
    shoal_dirs::{Contents, ShoalDir},
};

impl Reviews {
    /// Hold the run of review `id` of `repository` at `place`, its
    /// subject's, once no earlier run there holds it, as a cancelled
    /// review's does while it ends; while a run that could not be removed
    /// keeps the place, until a restarted daemon removes it, the review runs
    /// beside it instead.
    pub(crate) async fn hold_run(
        &self,
        id: ReviewId,
        repository: &RepositoryId,
        place: Files,
    ) -> Result<HeldRun<'_>> {
        let mut ended = self.ended.subscribe();
        loop {
            match self.try_hold_run(id, repository, &place.directory) {
                Ok(checkout) if !place.directory.exists() => {
                    return Ok(HeldRun {
                        files: place,
                        checkout,
                        at_place: true,
                    });
                }
                Ok(checkout) => {
                    drop(checkout);
                    break;
                }
                Err(true) => break,
                Err(false) => ended.changed().await.context("the daemon is stopping")?,
            }
        }
        log!(
            "review {id}: an earlier review left its run at {} that could not be removed; this \
             review runs beside it in a new session until `shoal daemon restart` removes it",
            place.directory.display()
        );
        let files = run_beside(&place, id);
        let checkout = (self.try_hold_run(id, repository, &files.directory))
            .map_err(|_| anyhow!("{} is in use", files.directory.display()))?;
        Ok(HeldRun {
            files,
            checkout,
            at_place: false,
        })
    }
}

/// A review's held run, and whether it is at its subject's place, where
/// the subject's session is found.
pub(crate) struct HeldRun<'a> {
    pub(crate) files: Files,
    pub(crate) checkout: CheckoutHold<'a>,
    pub(super) at_place: bool,
}

/// Remove review `id`'s run and give back its checkout and admission. A run whose
/// reviewer `left_running` processes keeps all three, as does a run that
/// could not be removed its checkout, until a restarted daemon removes them.
pub(crate) async fn release_run(
    id: ReviewId,
    files: &Files,
    checkout: CheckoutHold<'_>,
    admission: Box<dyn Admission + '_>,
    left_running: bool,
) {
    if left_running {
        log!("review {id}: its reviewer's processes could not be stopped");
        checkout.keep();
        admission.keep();
        return;
    }
    if let Err(error) = remove_run(files).await {
        log!("review {id}: {error:#}");
        checkout.keep();
    }
}

/// The runs a stopped daemon left in `directory`.
pub(super) fn run_directories(directory: &Path) -> Result<Vec<Files>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    let mut runs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            runs.push(Files {
                directory: entry.path(),
            });
        }
    }
    Ok(runs)
}

/// The files of `review`'s run, in the reviewed repository's directory, at
/// one place for each workspace and each PR reviewed in it, so its reviewers
/// run in the same directory each time, where an AI tool finds the sessions
/// it saved there.
pub(crate) fn run_files(repository: &Repository, review: &Review) -> Result<Files> {
    let workspace = review.workspace_id.as_str();
    let place = match &review.target {
        ReviewTarget::Workspace => workspace.to_owned(),
        ReviewTarget::Pr { url, .. } => format!("{workspace}-pr-{}", url.pull()?),
    };
    Ok(Files {
        directory: review_runs(repository).root().join(place),
    })
}

/// The files of review `id`'s run beside `place`, its subject's, which a
/// run that could not be removed keeps.
pub(super) fn run_beside(place: &Files, id: ReviewId) -> Files {
    let mut name = place.directory.file_name().unwrap_or_default().to_owned();
    name.push(format!("-{id}"));
    Files {
        directory: place.directory.with_file_name(name),
    }
}

/// The directory of the repository's review runs.
pub(super) fn review_runs(repository: &Repository) -> ShoalDir {
    ShoalDir::of(repository, Contents::ReviewRuns)
}

/// Remove a run's checkout and files.
pub(super) async fn remove_run(files: &Files) -> Result<()> {
    git::detached::remove(&files.checkout()).await?;
    match std::fs::remove_dir_all(&files.directory) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("remove {}", files.directory.display()))
        }
        _ => Ok(()),
    }
}
