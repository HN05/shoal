//! Change a PR's base branch, for stacked workspaces whose base PR merged.
use anyhow::{Context, Result};

use crate::{
    ForgeRepo,
    account::Account,
    items::{
        action::{Action, Edit},
        link::ItemKind,
    },
};

impl ForgeRepo {
    /// Retargeting follows the user's merge, so it uses the user's login.
    pub async fn retarget(&self, account: &Account, number: u64, base: &str) -> Result<()> {
        let edit = Action::Edit(Edit {
            base: Some(base.to_owned()),
            ..Edit::default()
        });
        self.act(account, ItemKind::Pr, number, &edit)
            .await
            .with_context(|| format!("could not retarget PR #{number} to {base}"))?;
        Ok(())
    }
}
