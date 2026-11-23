//! Importing a space another Shoal exported: recording its removal record
//! here as its kind needs, then restoring it as `shoal restore` does.
use crate::Spaces;
use crate::daemon::Daemon;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::repository::Selector;
use shoal_protocol::transcripts::CarriedSessionFile;
use shoal_refs::CommitId;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

use crate::daemon::ClaimFailure;
use crate::refusal::{Reason, Refusal};
use crate::transfer::Claim;
use crate::{
    git,
    model::{
        Head, RefPart, RemovedKind, RemovedSpace, Repository, RestoredSpace, SpaceRef, WorkspaceRef,
    },
    time::UnixTime,
};
use shoal_protocol::space_names::SpaceName;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::{BTreeMap, BTreeSet};

/// A space of a package to record as removed here.
#[derive(Debug, Clone)]
pub struct SpaceImport {
    /// The repository it goes to; none for a chat without one.
    pub repository: Option<Selector>,
    /// A Git bundle of a workspace's checkpoint, whose commits it fetches.
    pub bundle: Option<PathBuf>,
    /// A commit of the default branch the bundle builds on, which the
    /// repository fetches when it lacks it.
    pub prerequisite: Option<String>,
    /// Its removal record, with the agents that continue here.
    pub space: RemovedSpace,
    /// Set for a copy, which opens as a new space, a workspace on a branch
    /// of its own.
    pub copy: bool,
    /// The agents' session files, which the daemon keeps with the record.
    pub session_files: Vec<CarriedSessionFile>,
    /// The copies this import made before it of the other spaces of its
    /// package, by the original's ID, which stand in for them as its
    /// parent and, for a workspace on its parent's branch, as the branch
    /// it builds on.
    pub copies: BTreeMap<SpaceId, SpaceId>,
    /// The other spaces of its package recorded before it, by the ID each
    /// has here, which stay its parent and the workspace a chat ran in
    /// though they open only after this import.
    pub recorded: BTreeSet<SpaceId>,
    /// How it claims its space at the hub.
    pub claim: Claim,
}

impl Spaces {
    /// Open a space another Shoal exported, as `import` describes: record
    /// it as [`Self::record_import`] does and restore it under its ID,
    /// below its parent where that is here, a copy under an ID of its own.
    /// A failed restore leaves the record for `shoal restore`.
    pub async fn import_space(
        &self,
        daemon: &dyn Daemon,
        import: SpaceImport,
    ) -> Result<RestoredSpace> {
        let (removed, warnings) = self.record_import(daemon, import).await?;
        let id = removed.space_id;
        let mut restored = (self.reopen_space(daemon, &SpaceSelector::id(&id), None))
            .await
            .with_context(|| {
                format!("the imported space is recorded as removed; shoal restore {id} retries")
            })?;
        restored.warnings_mut().extend(warnings);
        Ok(restored)
    }

    /// Record the space `import` packages as removed here, with a
    /// workspace's checkpoint fetched from its bundle or else on the
    /// default branch, and its agents' session files, and claim it at the
    /// hub, so it takes the ID over from the machine it moves from; with
    /// the warnings of what it changed. An error leaves no record. The
    /// claim of a move through the hub, or of a takeover, makes the space
    /// this machine's, so one the hub refused forgets the record, and an
    /// import whose claim stood, or may have, keeps the record for
    /// `shoal restore`.
    pub async fn record_import(
        &self,
        daemon: &dyn Daemon,
        import: SpaceImport,
    ) -> Result<(RemovedSpace, Vec<String>)> {
        let id = import.space.space_id.clone();
        let claim = import.claim.clone();
        let recorded = (self.record_package(daemon, import).await)
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        let claimed = match &claim {
            Claim::Plain => {
                // The machine it moves from keeps it until this import is done.
                daemon.claim_space(&id).await;
                return Ok(recorded);
            }
            Claim::Move(ticket) => daemon.claim_moved(&id, ticket).await,
            Claim::Takeover => daemon.claim_taken_over(&id).await,
        };
        match claimed {
            Ok(()) => Ok(recorded),
            Err(ClaimFailure::Refused(error)) => {
                self.forget_unclaimed(&id).await;
                let error = error.context("the hub took no claim of it");
                Err(Refusal::wrap(Reason::NotImported, error))
            }
            Err(ClaimFailure::Unknown(error)) => Err(error.context(format!(
                "the hub may have taken this machine's claim of it, so its record stays here for shoal restore {id}"
            ))),
        }
    }

    /// Forget the removal record an import of the space `id` made, whose
    /// claim the hub refused; its checkpoint and kept session files expire
    /// with no record to own them.
    async fn forget_unclaimed(&self, id: &SpaceId) {
        let space = id.clone();
        let forgotten = (self.store).write(move |tx| {
            tx.execute("DELETE FROM removed_spaces WHERE space_id=?1", [&space])?;
            Ok(())
        });
        if let Err(error) = forgotten.await {
            crate::log!("forget the unclaimed import of space {id}: {error:#}");
        }
    }

    /// Record the space `import` packages as removed here, as
    /// [`Self::record_import`] does before it claims it.
    async fn record_package(
        &self,
        daemon: &dyn Daemon,
        import: SpaceImport,
    ) -> Result<(RemovedSpace, Vec<String>)> {
        self.ensure_not_here(&import.space.space_id, import.copy)
            .await?;
        let repo = match &import.repository {
            Some(selector) => Some(
                daemon
                    .repository(crate::repository::Selector::from(selector))
                    .await?,
            ),
            None => None,
        };
        let mut warnings = Vec::new();
        let prerequisite = import.prerequisite.as_deref();
        let space = self.below_copies(import.space, &import.copies).await?;
        let here = Here {
            repo: repo.as_ref(),
            recorded: &import.recorded,
        };
        let removed =
            (self.recorded_here(daemon, here, prerequisite, space, &mut warnings)).await?;
        // A repository move holds this gate; the checkout may have moved.
        let locked = match &repo {
            Some(repo) => Some(
                daemon
                    .lock_repository(crate::repository::Selector::from(&repo.id))
                    .await?,
            ),
            None => None,
        };
        let repo = locked.as_ref().map(|(repo, _git)| repo);
        let _refs = self.gates.lock_checkpoints().await;
        if let Some(kept) = removed.workspace() {
            let repo = repo.context("a workspace belongs to a repository")?;
            let bundle = import.bundle.as_deref();
            self.save_imported_checkpoint(repo, bundle, &kept.checkpoint.commit)
                .await?;
        }
        let removed = match import.copy {
            true => self.copied_record(repo, removed).await?,
            false => removed,
        };
        let removed = (self.record_imported(daemon, removed, &import.session_files)).await?;
        Ok((removed, warnings))
    }

    /// Refuse to import the packaged space `id` under its ID while a space
    /// here has that ID, whose kept session files and record the import
    /// would replace; a `copy` takes an ID of its own.
    pub(super) async fn ensure_not_here(&self, id: &SpaceId, copy: bool) -> Result<()> {
        if copy {
            return Ok(());
        }
        if let Some(here) = self.find_space(id).await? {
            let (name, number) = (&here.name, here.number);
            bail!(
                "{name} is already here as space {number}; shoal rm {number} removes it here, \
                 or shoal move --keep -o where it was exported writes a copy that opens beside it"
            );
        }
        Ok(())
    }

    /// The exported `removed` as [`Self::imported_record`] records it
    /// `here`, once a workspace's checkpoint is its own and its repository
    /// has the `prerequisite` its bundle builds on, a chat in the place it
    /// runs here, with a warning in `warnings` when that is not where it
    /// ran.
    async fn recorded_here(
        &self,
        daemon: &dyn Daemon,
        here: Here<'_>,
        prerequisite: Option<&str>,
        removed: RemovedSpace,
        warnings: &mut Vec<String>,
    ) -> Result<RemovedSpace> {
        let repo = here.repo;
        if let Some(kept) = removed.workspace() {
            ensure!(
                kept.checkpoint.workspace_id == removed.space_id,
                "the export's checkpoint belongs to another workspace"
            );
        }
        if let (Some(commit), Some(repo)) = (prerequisite, repo) {
            self.fetch_missing(daemon, repo, &commit.into()).await?;
        }
        let mut removed = self.imported_record(daemon, here, removed).await?;
        let recorded = here.recorded;
        warnings.extend(
            self.without_absent_worktree(daemon, &mut removed, recorded)
                .await,
        );
        Ok(removed)
    }

    /// Record that the imported chat `removed` runs in its repository's
    /// checkout when the workspace whose worktree it ran in is not ready
    /// here, which its restore would otherwise wait for in vain, with the
    /// warning that says so; a workspace among the `recorded` spaces of its
    /// package opens before it.
    async fn without_absent_worktree(
        &self,
        daemon: &dyn Daemon,
        removed: &mut RemovedSpace,
        recorded: &BTreeSet<SpaceId>,
    ) -> Option<String> {
        let RemovedKind::Chat { chat } = &mut removed.kind else {
            return None;
        };
        let workspace = chat.workspace.as_ref()?;
        if recorded.contains(&workspace.id) {
            return None;
        }
        let selector = SpaceSelector::id(&workspace.id);
        if self.chat_workspace(daemon, &selector).await.is_ok() {
            return None;
        }
        let warning = format!(
            "{} ran in the worktree of {}, which is not ready here, so it opens in the checkout",
            removed.name, workspace.name
        );
        chat.workspace = None;
        Some(warning)
    }

    /// The packaged `removed` with the copies among `copies`, which this
    /// import opened before it, in place of the originals it names: below
    /// the copy of the space it was below, for a workspace that builds on
    /// that space's branch building on the copy's branch, and for a chat in
    /// the worktree of the copy of the workspace it ran in.
    async fn below_copies(
        &self,
        mut removed: RemovedSpace,
        copies: &BTreeMap<SpaceId, SpaceId>,
    ) -> Result<RemovedSpace> {
        if let Some(parent) = removed.parent.clone()
            && let Some(copy) = self.copy_among(copies, &parent.id).await?
        {
            on_copied_base(&mut removed, &parent, &copy);
            removed.parent = Some(copy);
        }
        if let RemovedKind::Chat { chat } = &mut removed.kind
            && let Some(workspace) = &chat.workspace
            && let Some(copy) = self.copy_among(copies, &workspace.id).await?
        {
            chat.workspace = Some(WorkspaceRef {
                branch: copy.branch().cloned(),
                id: copy.id,
                name: copy.name,
            });
        }
        Ok(removed)
    }

    /// The copy among `copies` of the space `id`, when this import made
    /// one, open or recorded to open after it.
    async fn copy_among(
        &self,
        copies: &BTreeMap<SpaceId, SpaceId>,
        id: &SpaceId,
    ) -> Result<Option<SpaceRef>> {
        let Some(copy) = copies.get(id) else {
            return Ok(None);
        };
        if let Some(open) = self.find_space(copy).await? {
            return Ok(Some(open.space_ref()));
        }
        let recorded = (self.removed_space(&SpaceSelector::id(copy)).await)
            .with_context(|| format!("the copy of {id} is no longer here"))?;
        Ok(Some(recorded_ref(&recorded)))
    }

    /// Fetch the default branch when the repository lacks `commit`, which
    /// the export builds on.
    async fn fetch_missing(
        &self,
        daemon: &dyn Daemon,
        repo: &Repository,
        commit: &CommitId,
    ) -> Result<()> {
        if git::has_commit(&repo.path, commit.as_str()).await? {
            return Ok(());
        }
        daemon
            .fetch_default_branch(&repo.id)
            .await
            .context("fetch the default branch the export builds on")
    }

    /// The exported record as the repository's `here`, or a chat's without
    /// one, expiring after its retention as a removal here would, a
    /// workspace's checkpoint with it; a chat without a repository runs in
    /// `root_dir`, which this prepares as a chat's start does.
    async fn imported_record(
        &self,
        daemon: &dyn Daemon,
        here: Here<'_>,
        removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let repo = here.repo;
        let settings = daemon.space_settings_in(repo.map(|repo| &repo.id)).await?;
        let retention = settings.cleanup.restore.retention();
        let expires_at = UnixTime::now() + retention;
        // The space it was below where it was exported stays its parent only
        // where that space is here too, or opens after it as part of its
        // package, or runs on another joined machine.
        let parent = match removed.parent {
            Some(parent) if here.recorded.contains(&parent.id) => Some(parent),
            Some(parent) if self.find_space(&parent.id).await?.is_some() => Some(parent),
            Some(parent) if daemon.runs_elsewhere(&parent.id).await => Some(parent),
            _ => None,
        };
        let mut removed = RemovedSpace {
            repository_id: repo.map(|repo| repo.id.clone()),
            expires_at,
            parent,
            ..removed
        };
        match (&mut removed.kind, repo) {
            (RemovedKind::Workspace(kept), Some(repo)) => {
                kept.checkpoint.repository_id = repo.id.clone();
                kept.checkpoint.expires_at = expires_at;
            }
            (RemovedKind::Workspace(_), None) => bail!("a workspace belongs to a repository"),
            (RemovedKind::Chat { chat }, Some(_)) => chat.root_dir = None,
            (RemovedKind::Chat { chat }, None) => {
                chat.root_dir = Some(daemon.prepare_chat_root().await?);
            }
        }
        Ok(removed)
    }

    /// The exported `removed` as a new space that copies it, with its own
    /// ID; a workspace's as [`Self::copied_workspace`] says. The caller
    /// holds the repository's Git gate.
    async fn copied_record(
        &self,
        repo: Option<&Repository>,
        removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let removed = RemovedSpace {
            space_id: SpaceId::generate(),
            ..removed
        };
        match removed.workspace() {
            Some(_) => {
                let repo = repo.context("a workspace belongs to a repository")?;
                self.copied_workspace(repo, removed).await
            }
            None => Ok(removed),
        }
    }

    /// The exported workspace `removed`, which copies another under its ID,
    /// with a checkpoint of its own, on a branch of its own named after the
    /// original's, created at its HEAD, and without the PRs and CI runs,
    /// which stay with the original's branch. The caller holds the
    /// repository's Git gate, so a concurrent import takes another branch.
    async fn copied_workspace(
        &self,
        repo: &Repository,
        mut removed: RemovedSpace,
    ) -> Result<RemovedSpace> {
        let id = removed.space_id.clone();
        let kept = removed.kept_workspace_mut()?;
        kept.checkpoint.workspace_id = id;
        let name = match kept.head.clone() {
            Head::Branch(branch) => {
                let branch = self.copy_branch(repo, branch.as_str()).await?;
                git::branch::create(&repo.path, &branch, kept.checkpoint.head.as_str()).await?;
                kept.checkpoint.branch = Some(branch.as_str().into());
                kept.head = Head::Branch(branch.clone());
                SpaceName::derive(branch.as_str(), "workspace")
            }
            Head::Commit(_) => removed.name.clone(),
        };
        Ok(RemovedSpace {
            name,
            links: removed.links.copied(),
            ..removed
        })
    }

    /// Point the ref of the imported checkpoint `commit` at it, fetching it
    /// from `bundle`, or else from the default branch. The caller holds the
    /// repository's Git gate and the checkpoint gate.
    async fn save_imported_checkpoint(
        &self,
        repo: &Repository,
        bundle: Option<&Path>,
        commit: &CommitId,
    ) -> Result<()> {
        let reference = git::checkpoint::reference(commit);
        match bundle {
            Some(bundle) => git::bundle::fetch(&repo.path, bundle, &reference).await,
            None => git::checkpoint::save(&repo.path, &reference, commit)
                .await
                .with_context(|| format!("the default branch has no commit {commit}")),
        }
    }

    /// Keep the agents' `session_files` for `removed`, then record it in one
    /// transaction, as a removal would: a workspace's checkpoint with it. A
    /// failure keeps neither. The caller holds the checkpoint gate, and for
    /// a space of a repository its Git gate.
    async fn record_imported(
        &self,
        daemon: &dyn Daemon,
        removed: RemovedSpace,
        session_files: &[CarriedSessionFile],
    ) -> Result<RemovedSpace> {
        let removed = self
            .keep_imported_sessions(daemon, removed, session_files)
            .await?;
        let record = removed.clone();
        if let Err(error) = self.store.write(move |tx| record_import(tx, record)).await {
            let _ = self.discard_imported_sessions(daemon, &removed).await;
            return Err(error);
        }
        Ok(removed)
    }
}

/// Where an import records a space: in its repository, none for a chat
/// without one, beside the other spaces of its package it `recorded` before.
#[derive(Clone, Copy)]
struct Here<'a> {
    repo: Option<&'a Repository>,
    recorded: &'a BTreeSet<SpaceId>,
}

/// A reference to the space `removed` records, as it opens.
fn recorded_ref(removed: &RemovedSpace) -> SpaceRef {
    let part = match removed.workspace() {
        Some(kept) => RefPart::Workspace {
            branch: match &kept.head {
                Head::Branch(branch) => Some(branch.clone()),
                Head::Commit(_) => None,
            },
        },
        None => RefPart::Chat,
    };
    SpaceRef {
        id: removed.space_id.clone(),
        name: removed.name.clone(),
        part,
    }
}

/// Make the workspace `removed` records, which builds on the branch of its
/// `parent`, build on the branch of the parent's `copy` instead.
fn on_copied_base(removed: &mut RemovedSpace, parent: &SpaceRef, copy: &SpaceRef) {
    if let RemovedKind::Workspace(kept) = &mut removed.kind
        && let Some(base) = &mut kept.base
        && let (Some(original), Some(branch)) = (parent.branch(), copy.branch())
        && base.reference.as_deref() == Some(git::local_ref(original).as_str())
    {
        base.reference = Some(git::local_ref(branch));
    }
}

/// Record an imported space's removal record, a workspace's checkpoint under
/// its next number with the record naming that number, unless a space here
/// has its ID or its repository is being removed.
fn record_import(tx: &rusqlite::Transaction<'_>, mut removed: RemovedSpace) -> Result<()> {
    let id = &removed.space_id;
    ensure!(
        !crate::store::exists(tx, "SELECT 1 FROM spaces WHERE id=?1", [id])?,
        "a space with the ID {id} is already here"
    );
    if let Some(repository) = &removed.repository_id {
        ensure!(
            !crate::store::repository_removing(tx, repository)?,
            "the repository is being removed"
        );
    }
    if let RemovedKind::Workspace(kept) = &mut removed.kind {
        let checkpoint = &mut kept.checkpoint;
        checkpoint.number = super::super::workspace::checkpoints::insert_numbered(tx, checkpoint)?;
    }
    super::records::record(tx, &removed)
}
