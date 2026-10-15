//! Change a PR's base branch, for stacked workspaces whose base PR merged.
use anyhow::{Context, Result};

use super::{
    ForgeRepo,
    account::Account,
    action::{Action, Edit},
    link::ItemKind,
};

impl ForgeRepo {
    /// Retargeting follows the user's merge, so it uses the user's login.
    pub(crate) async fn retarget(&self, account: &Account, number: u64, base: &str) -> Result<()> {
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
