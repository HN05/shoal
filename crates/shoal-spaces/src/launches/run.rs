//! A launch on a CI run: the run read before the start is placed, as the
//! caller may link it, which settles the launch into the start the run
//! names, its PR's, its branch's or a chat's, so the rest of the start goes
//! as for those; the run is linked to the space once the start has one.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::launches::{Launch, StartTarget};
use shoal_protocol::protocol::ItemContext;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;

use crate::{
    forge::{ci::run_action::Run, items::reference::ItemRef},
    git::{self, default_branch::DefaultBranchLookup},
    model::{Repository, SpaceKind},
    scope::Caller,
};
use shoal_core::launches::{
    self,
    run::{RunStart, ensure_run_fits, named_run},
};

impl Spaces {
    /// Settle `launch` for `caller` on the CI run it names, if any: the
    /// run, read in its registered repository, which the launch must not
    /// name otherwise, starts in a chat in a mode that starts one, and
    /// otherwise where [`RunStart`] says.
    pub(super) async fn settle_launch_run(
        &self,
        daemon: &dyn Daemon,
        launch: &mut Launch,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let Some(url) = named_run(launch)?.cloned() else {
            return Ok(());
        };
        ensure_run_fits(launch)?;
        let repo = self.run_repository(daemon, launch, &url).await?;
        let context = run_context(&repo, caller);
        let run = daemon.launch_run(&context, &url, caller).await?;
        let start = match self.mode_starts_chat(daemon, launch, caller).await? {
            true => RunStart::Unbranched,
            false => RunStart::of(&run, pushed_branch(&repo, &run).await?),
        };
        launches::run::settle(launch, (url, &run), start, repo.id.to_string())
    }

    /// The registered repository of the run `url` that `launch` names: the
    /// one it names with `--repo` or positionally, which must be the
    /// run's, else the run's own.
    async fn run_repository(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        url: &ItemRef,
    ) -> Result<Repository> {
        let repos = daemon.repositories().await?;
        let positional = match &launch.target {
            Some(StartTarget::Repository(named) | StartTarget::Number(named)) => Some(named),
            _ => None,
        };
        let Some(named) = launch.repository.as_ref().or(positional) else {
            let retry = "shoal start <run-url> --repo <repository>";
            let repo = crate::repository::with_remote(&repos, url.repository().clone(), retry);
            return Ok(repo.await?.clone());
        };
        let directory = launch.directory.as_deref();
        let selector = crate::repository::named_in(named.clone(), directory)?;
        let repo = crate::repository::select(&repos, selector).await?.clone();
        launches::workspace::ensure_link_in(&repo, Some(url.repository())).await?;
        Ok(repo)
    }

    /// Whether the mode `launch` names starts a chat for `caller`; the
    /// default mode starts a workspace.
    async fn mode_starts_chat(
        &self,
        daemon: &dyn Daemon,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<bool> {
        let Some(mode) = &launch.mode else {
            return Ok(false);
        };
        let (kind, _) = self.launch_kind(daemon, launch, mode, caller).await?;
        Ok(kind == SpaceKind::Chat)
    }

    /// Link the CI run a launch was settled on, if any, to the space `id`
    /// it started for `caller`, related to it. The space started whatever
    /// happens here, so a failure joins the start's `warnings`.
    pub(super) async fn link_launch_run(
        &self,
        daemon: &dyn Daemon,
        run: Option<ItemRef>,
        (id, warnings): (&SpaceId, &mut Vec<String>),
        caller: Option<&Caller>,
    ) {
        let Some(run) = run else {
            return;
        };
        let space = SpaceSelector::id(id);
        if let Err(error) = daemon.link_related_run(&space, &run, caller).await {
            warnings.push(format!(
                "could not link {run}: {error:#}; link it with shoal link ci {run} related"
            ));
        }
    }
}

/// Where `caller` reads a run of `repo` it starts on: from its own space,
/// whose repository decides whether the run is its own to link, or for the
/// user from `repo`'s checkout.
fn run_context(repo: &Repository, caller: Option<&Caller>) -> ItemContext {
    match caller {
        Some(caller) => ItemContext::space(SpaceSelector::id(&caller.space_id)),
        None => ItemContext::Checkout(repo.id.clone()),
    }
}

/// The branch of `repo` other than its default branch that `run` ran on,
/// where it ran for no PR and its remote has the branch; none for a tag,
/// which a forge reports in a branch's place.
async fn pushed_branch(repo: &Repository, run: &Run) -> Result<Option<String>> {
    let default = git::default_branch::resolve(&repo.path, DefaultBranchLookup::Cached).await?;
    let Some(other) = RunStart::other_ref(run, default.as_str()) else {
        return Ok(None);
    };
    let branch = BranchName::from(other);
    let pushed = git::existing_branch::on_remote(&repo.path, "origin", &branch).await?;
    Ok(pushed.then(|| other.to_owned()))
}
