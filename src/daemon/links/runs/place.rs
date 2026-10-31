//! Where a CI run request acts: the repository of the workspace or
//! registered checkout it comes from as its agent account, or another
//! repository a run or workflow URL names as the user's own logins, which
//! workspace processes may not use.
use anyhow::{Context, Result, ensure};

use crate::{
    daemon::workspace::Manager,
    forge::{ForgeRepo, IssueInput, account::Account},
    model::Workspace,
    protocol::ItemContext,
};

/// The repository a CI run request acts in and the account it acts as.
pub(in crate::daemon::links) struct RunPlace {
    /// The workspace the request came from, if any.
    pub workspace: Option<Workspace>,
    pub forge: ForgeRepo,
    pub account: Account,
    /// Whether `forge` is the repository the request came from.
    pub own: bool,
}

/// The workspace `selector` names, if any, as the context of a request.
pub(in crate::daemon::links) fn workspace_context(selector: Option<&str>) -> Option<ItemContext> {
    selector.map(|selector| ItemContext::Workspace(selector.to_owned()))
}

impl RunPlace {
    /// The workspace whose own repository the request acts in.
    pub fn own_workspace(&self) -> Option<&Workspace> {
        self.workspace.as_ref().filter(|_| self.own)
    }
}

impl Manager {
    /// Where a request from `context`, if any, acts on the run or workflow
    /// of `named`, the repository its URL names, if any: the repository of
    /// the workspace or checkout unless `named` is another, which a `scoped`
    /// caller may not act in.
    pub(in crate::daemon::links) async fn run_place(
        &self,
        context: Option<&ItemContext>,
        named: Option<ForgeRepo>,
        scoped: bool,
    ) -> Result<RunPlace> {
        let place = match context {
            Some(context) => Some(self.place(context).await?),
            None => None,
        };
        let workspace = place.as_ref().and_then(|place| place.workspace().cloned());
        if let Some(place) = &place {
            let (own, account) = self.place_forge_account(place).await?;
            if named.as_ref().is_none_or(|named| *named == own) {
                return Ok(RunPlace {
                    workspace,
                    forge: own,
                    account,
                    own: true,
                });
            }
        }
        let forge = named.context("outside a workspace, name it by its URL")?;
        ensure!(
            !scoped,
            "workspace processes act only on CI runs and workflows of their own repository"
        );
        Ok(RunPlace {
            workspace,
            forge,
            account: self.user_account(),
            own: false,
        })
    }

    /// Where a request acts on the run `input`, its web URL or the number
    /// in it, names.
    pub(in crate::daemon::links) async fn run_input_place(
        &self,
        context: Option<&ItemContext>,
        input: &str,
        scoped: bool,
    ) -> Result<RunPlace> {
        let named = self.named_run_forge(input).await?;
        self.run_place(context, named, scoped).await
    }

    /// The repository the run URL `input` names, or none for a run number.
    pub(in crate::daemon::links) async fn named_run_forge(
        &self,
        input: &str,
    ) -> Result<Option<ForgeRepo>> {
        Ok(match IssueInput::parse(input) {
            IssueInput::Url => Some(self.run_url_forge(input).await?),
            _ => None,
        })
    }
}
