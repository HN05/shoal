//! The spaces a package carries with its space, which the import opens
//! again after it, parents first: under their IDs, or as copies below the
//! copies of their parents, stopped or with their agents' restores queued.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result};

use super::{Package, restored_id};
use crate::cli::commands::restore::reopen::{self, ReopenedExt};
use crate::{
    cli::{context::Context, ui},
    model::RemovedSpace,
    transfer::{CarriedSpace, ImportTarget, Manifest, ManifestExt, PackagedSpaceExt, SpaceImport},
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::space_ids::SpaceId;

/// A space a package carries, as it opens here: the repository it goes
/// to, none for a chat without one, the commit of the default branch its
/// bundle builds on, and its removal record, with the sessions its agents
/// keep here.
pub(super) struct CarriedImport {
    repository: Option<crate::repository::Selector>,
    prerequisite: Option<String>,
    space: RemovedSpace,
}

impl CarriedImport {
    /// The sessions its agents continue here.
    pub(super) fn sessions(&self) -> impl Iterator<Item = &AgentSessionId> {
        let stopped = self.space.agents.iter().map(|agent| &agent.record.session);
        let last = self.space.last_sessions.iter().map(|last| &last.session);
        stopped.chain(last).flatten()
    }

    /// What the import of the package's own space checks of it before
    /// recording anything.
    pub(super) fn target(&self) -> ImportTarget {
        ImportTarget {
            repository: self.repository.clone(),
            space: self.space.space_id.clone(),
        }
    }
}

/// The spaces `manifest` carries, as they open here, each in the
/// repository [`destination`] names when the package goes to `repository`,
/// keeping their sessions as `sessions` says.
pub(super) fn carried_here(
    manifest: &Manifest,
    repository: Option<&crate::repository::Selector>,
    sessions: Option<bool>,
) -> Result<Vec<CarriedImport>> {
    let machine = crate::process::machine::id();
    let mut carried = Vec::new();
    for space in &manifest.carried {
        let name = &space.space.record.name;
        let repository = (destination(manifest, space, repository))
            .with_context(|| format!("{name} goes with it, but"))?;
        let kept = manifest.carried_sessions_on(space, machine.as_deref(), sessions);
        carried.push(CarriedImport {
            repository,
            prerequisite: space.space.prerequisite.clone(),
            space: space.space.record_with(kept),
        });
    }
    Ok(carried)
}

/// The repository the `carried` space of `manifest` goes to: none for a
/// chat without one, `repository`, where its package goes, for a space of
/// the package's repository, else the repository with its own origin.
fn destination(
    manifest: &Manifest,
    carried: &CarriedSpace,
    repository: Option<&crate::repository::Selector>,
) -> Result<Option<crate::repository::Selector>> {
    let Some(own) = &carried.space.record.repository_id else {
        return Ok(None);
    };
    if manifest.space.record.repository_id.as_ref() == Some(own) {
        return Ok(repository.cloned());
    }
    let origin = (carried.remote_url.clone()).context("its repository has no origin")?;
    Ok(Some(ui::repository_selector(origin)?))
}

/// Open each space `package` carries again, after the package's own space
/// `opened` opened, with the session files among those it carries that
/// their agents keep, queueing their agents' restores when `restore`; a
/// space that cannot open is a warning, since the package's space opened,
/// and its record stays for `shoal restore`.
pub(super) async fn open(ctx: &Context, package: &Package, opened: &SpaceId, restore: bool) {
    let mut copies = BTreeMap::new();
    if package.copy {
        copies.insert(package.space.record.space_id.clone(), opened.clone());
    }
    for import in &package.carried {
        if let Err(error) = open_one(ctx, import, package, &mut copies, restore).await {
            eprintln!(
                "warning: cannot open {} again: {error:#}",
                import.space.name
            );
        }
    }
}

/// Open `import` again as [`open`] does, below the `copies` of the spaces
/// a copy was below, adding its own, and report it.
async fn open_one(
    ctx: &Context,
    import: &CarriedImport,
    package: &Package,
    copies: &mut BTreeMap<SpaceId, SpaceId>,
    restore: bool,
) -> Result<()> {
    let kept: Vec<_> = import.sessions().collect();
    let id = &import.space.space_id;
    let space = SpaceImport {
        repository: import.repository.clone(),
        bundle: package.bundle_of(id),
        prerequisite: import.prerequisite.clone(),
        space: import.space.clone(),
        copy: package.copy,
        session_files: package.carried_for(&kept),
        copies: copies.clone(),
        carried: Vec::new(),
    };
    let restored = crate::client::transfer::import(&ctx.paths, space).await?;
    if package.copy {
        copies.insert(id.clone(), restored_id(&restored).clone());
    }
    let mut reopened = reopen::prepare_set_up(ctx, restored).await?;
    let post_setup = reopen::run_post_setup(ctx, &reopened).await;
    if post_setup.is_ok() && restore {
        reopened.queue_agents(ctx).await;
    }
    if !ctx.json {
        outln!("{}", reopened.message(ctx, "Imported"));
    }
    post_setup.context("it opened, but its hooks.post_setup failed")
}
