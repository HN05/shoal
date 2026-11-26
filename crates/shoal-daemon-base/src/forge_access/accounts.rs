//! The accounts requests to a forge go out as, which `docs/design/forge.md`
//! assigns. Each remembers what the forge's responses taught it.
use shoal_core::config::forges::{Forges, Whose};
use shoal_forge::transport::account::Account;

use super::ForgeAccess;
use crate::callers::Caller;

/// What names the account of the user's own tokens, `forges` sets, in
/// forge memories.
fn user_identity(forges: &Forges) -> String {
    format!("user {}", forges.identity(Whose::User))
}

impl ForgeAccess {
    /// The user's own logins, which reads and the user's decisions use.
    pub fn user_account(&self) -> Account {
        let forges = &self.config.current().machine.forges;
        let values = self.config.values();
        Account::user(forges.logins(Whose::User, values))
            .remembering(self.memories.of(&user_identity(forges)))
            .naming_missing(Whose::User.missing())
    }

    /// The agent account: the hosts' agent tokens, and the user's own
    /// logins for hosts without one.
    pub fn agent_account(&self) -> Account {
        let forges = &self.config.current().machine.forges;
        let values = self.config.values();
        // An agent with the user's logins shares what the user's requests taught.
        let tokens = forges.identity(Whose::Agent);
        let user = user_identity(forges);
        let identity = if tokens.is_empty() {
            user
        } else {
            format!("agent {tokens} {user}")
        };
        Account::agent(
            forges.logins(Whose::Agent, values),
            forges.logins(Whose::User, values),
        )
        .remembering(self.memories.of(&identity))
        .naming_missing(Whose::Agent.missing())
    }

    /// The account scoped callers approve and request changes on PRs as:
    /// the hosts' review tokens, with no other login to fall back on.
    pub fn review_account(&self) -> Account {
        let forges = &self.config.current().machine.forges;
        let identity = format!("review {}", forges.identity(Whose::Review));
        Account::user(forges.logins(Whose::Review, self.config.values()))
            .remembering(self.memories.of(&identity))
            .naming_missing(Whose::Review.missing())
    }

    /// The account `caller`'s read goes out as: the agent account for a
    /// scoped caller and the user's own logins for the user.
    pub fn read_account(&self, caller: Option<&Caller>) -> Account {
        match caller {
            Some(_) => self.agent_account(),
            None => self.user_account(),
        }
    }
}
