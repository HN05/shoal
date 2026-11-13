//! The forge assignees of the issues spaces link as assigned: the account a
//! space acts as joins them when it links an issue.
use shoal_states::states;

use crate::{
    daemon::{links::space::LinkSpace, scope::Caller, workspace::Manager},
    forge::{ForgeRepo, account::Account, items::reference::ItemRef},
};

states!(
    /// Whose account assigns an issue: the user's own, for the user's
    /// terminal, or the agent account, for a scoped caller.
    Assigner {
        User => "user",
        Agent => "agent",
    }
);

impl Assigner {
    /// The account `caller` assigns as.
    pub(crate) fn of(caller: Option<&Caller>) -> Self {
        match caller {
            Some(_) => Self::Agent,
            None => Self::User,
        }
    }
}

impl Manager {
    /// Assign the issue `url`, of the repository of `forge`, which `space`
    /// just linked as assigned, to the account of `assigner`, where its
    /// `[issue] assign` says to. Returns what the caller hears: the issue's
    /// other assignees, or why it was not assigned.
    pub(super) async fn assign_on_forge(
        &self,
        space: &LinkSpace,
        forge: &ForgeRepo,
        url: &ItemRef,
        assigner: Assigner,
    ) -> Vec<String> {
        match self.space_settings(space).await {
            Ok(settings) if !settings.issue.assign => return Vec::new(),
            Ok(_) => {}
            Err(error) => return vec![unassigned(url, &error)],
        }
        match forge.assign_own(&self.account_of(assigner), url).await {
            Ok(Some(assignment)) if !assignment.others.is_empty() => {
                let others = assignment.others.join(", ");
                vec![format!("{url} is also assigned to {others}")]
            }
            Ok(_) => Vec::new(),
            Err(error) => vec![unassigned(url, &error)],
        }
    }

    fn account_of(&self, assigner: Assigner) -> Account {
        match assigner {
            Assigner::User => self.user_account(),
            Assigner::Agent => self.agent_account(),
        }
    }
}

/// Why the issue `url` was linked but not assigned on the forge.
fn unassigned(url: &ItemRef, error: &anyhow::Error) -> String {
    format!("{url} is linked, but assigning it on the forge failed: {error:#}")
}

#[cfg(test)]
mod tests;
