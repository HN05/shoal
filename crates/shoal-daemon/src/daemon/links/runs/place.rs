//! Where a CI run request acts: the repository of the workspace or
//! registered checkout it comes from as its account there, or another
//! repository `--repo` or a run's or workflow's URL names, which a scoped
//! caller only reads, and the branch a run starts on there.
use anyhow::{Context, Result, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

use crate::{
    daemon::{
        links::{
            place::{Access, Place},
            target::ForgeTarget,
        },
        manager::Manager,
        scope::Caller,
    },
    forge::{
        ForgeRepo,
        items::{
            link::{LinkKind, Selection},
            reference::{ItemInput, ItemRef},
        },
    },
    model::Workspace,
    protocol::RepoTarget,
};
use shoal_forge::{
    account::Account,
    ci::workflow::{WorkflowRequests, is_workflow_url},
};
use shoal_protocol::space_names::SpaceSelector;

/// The refusal of a workspace process's watch of a run of another
/// repository.
pub(super) const ELSEWHERE: &str = "workspace processes watch only CI runs of their own repository";

/// The repository a CI run request acts in and the account it acts as.
pub(in crate::daemon::links) struct RunPlace {
    pub target: ForgeTarget,
    forge: ForgeRepo,
    pub account: Account,
}

impl RunPlace {
    /// The repository the request acts in.
    pub fn forge(&self) -> &ForgeRepo {
        &self.forge
    }

    /// The workspace whose own repository the request acts in.
    pub fn own_workspace(&self) -> Option<Workspace> {
        self.target.own_place().and_then(Place::workspace)
    }

    /// The repository the request acts in when it is the one the request
    /// came from.
    pub fn repository_id(&self) -> Option<&RepositoryId> {
        self.target.own_place().map(Place::repository_id)
    }

    /// The workspace or chat the request came from, which links the runs
    /// it starts.
    pub fn space_id(&self) -> Option<&SpaceId> {
        self.target.origin.as_ref().and_then(Place::space_id)
    }

    /// The branch a run starts on or is listed on: the `named` one,
    /// otherwise the workspace's in its own repository, or else the
    /// repository's default branch.
    pub async fn run_branch(&self, named: Option<String>) -> Result<String> {
        if let Some(branch) = named {
            return Ok(branch);
        }
        match self.own_workspace() {
            Some(workspace) => Ok((workspace
                .require_branch("a CI run without --branch")?
                .to_owned())
            .into()),
            None => (self.forge().default_branch(&self.account).await).with_context(|| {
                format!(
                    "could not read the default branch of {}; name one with --branch",
                    self.forge().repository_url()
                )
            }),
        }
    }
}

impl Manager {
    /// Where a request from `target` acts with `access` on the run or
    /// workflow of `named`, the repository its URL names, if any: as the
    /// account of the place it comes from in its own repository, and
    /// elsewhere as the caller to read and the user to change.
    pub(in crate::daemon::links) async fn run_place(
        &self,
        target: &RepoTarget,
        named: Option<ForgeRepo>,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<RunPlace> {
        let target = self.forge_target(target, named, access, caller).await?;
        let account = self.target_account(&target, access, caller);
        let forge = target.forge()?.clone();
        Ok(RunPlace {
            target,
            forge,
            account,
        })
    }

    /// Where a request acts with `access` on the run `input`, its web URL
    /// or the number in it, names, and the run.
    pub(in crate::daemon::links) async fn named_run(
        &self,
        target: &RepoTarget,
        input: &ItemInput,
        access: Access,
        caller: Option<&Caller>,
    ) -> Result<(RunPlace, ItemRef)> {
        let named = shoal_links::forges::of_input(self, input).await?;
        let place = self.run_place(target, named, access, caller).await?;
        let run = ItemRef::in_repository(place.forge(), LinkKind::Ci, input)?;
        Ok((place, run))
    }

    /// Where a request starts the workflow `input` names, by its file or ID
    /// or by its web URL, with the workflow and the branch the URL names.
    pub(in crate::daemon::links) async fn workflow_place(
        &self,
        target: &RepoTarget,
        input: Option<String>,
        caller: Option<&Caller>,
    ) -> Result<(RunPlace, Option<String>, Option<String>)> {
        let Some(url) = input.as_deref().filter(|input| is_workflow_url(input)) else {
            let place = self.run_place(target, None, Access::Change, caller).await?;
            return Ok((place, input, None));
        };
        let hosts = self.forge_hosts();
        let (named, link) = ForgeRepo::resolve_workflow_url(url, &hosts).await?;
        let place = (self.run_place(target, Some(named), Access::Change, caller)).await?;
        Ok((place, Some(link.workflow), link.branch))
    }

    /// Refuse a workspace process the explicitly selected run of another
    /// repository than its space's, which a watch would read with the user's
    /// own logins. A chat without a repository counts every registered
    /// repository as its own, and its watch reads no other.
    pub(in crate::daemon) async fn ensure_own_repository_run(
        &self,
        selector: &SpaceSelector,
        selection: &Selection,
    ) -> Result<()> {
        let (Some(LinkKind::Ci), Some(input)) = (selection.kind, selection.input.as_ref()) else {
            return Ok(());
        };
        let Some(named) = shoal_links::forges::of_input(self, input).await? else {
            return Ok(());
        };
        if self.space(selector).await?.repository_id.as_ref().is_none() {
            return Ok(());
        }
        let space = self.link_space(selector).await?;
        ensure!(
            named == shoal_links::forges::of_space(self, &space).await?,
            ELSEWHERE
        );
        Ok(())
    }
}
