//! Change a PR's base branch, for stacked workspaces whose base PR merged.
use anyhow::{Context, Result};
use shoal_refs::BranchName;

use crate::{
    ForgeRepo,
    account::Account,
    items::{
        action::{Action, Edit},
        link::ItemKind,
    },
    numbers::PullNumber,
};

impl ForgeRepo {
    /// Retargeting follows the user's merge, so it uses the user's login.
    pub async fn retarget(
        &self,
        account: &Account,
        number: PullNumber,
        base: &BranchName,
    ) -> Result<()> {
        let edit = Action::edit(Edit {
            base: Some(base.clone()),
            ..Edit::default()
        });
        self.act(account, ItemKind::Pr, number.get(), &edit)
            .await
            .with_context(|| format!("could not retarget PR #{number} to {base}"))?;
        Ok(())
    }
}
