//! The accounts requests to a forge go out as: the user's own logins for
//! reads and the user's decisions, the review account for a scoped
//! caller's verdicts, the agent account for the rest. Each remembers what
//! the forge's responses taught it.
use shoal_core::config::forges::{Forges, Whose};
use shoal_forge::account::Account;

use crate::Links;

/// What names the account of the user's own tokens, `forges` sets, in
/// forge memories.
fn user_identity(forges: &Forges) -> String {
    format!("user {}", forges.identity(Whose::User))
}

impl Links {
    /// The user's own logins, which reads and the user's decisions use.
    pub fn user_account(&self) -> Account {
        let forges = &self.config.current().machine.forges;
        Account::user(forges.logins(Whose::User))
            .remembering(self.memories.of(&user_identity(forges)))
            .naming_missing(Whose::User.missing())
    }

    /// The agent account: the hosts' agent tokens, and the user's own
    /// logins for hosts without one.
    pub fn agent_account(&self) -> Account {
        let forges = &self.config.current().machine.forges;
        // An agent with the user's logins shares what the user's requests taught.
        let tokens = forges.identity(Whose::Agent);
        let user = user_identity(forges);
        let identity = if tokens.is_empty() {
            user
        } else {
            format!("agent {tokens} {user}")
        };
        Account::agent(forges.logins(Whose::Agent), forges.logins(Whose::User))
            .remembering(self.memories.of(&identity))
            .naming_missing(Whose::Agent.missing())
    }

    /// The account scoped callers approve and request changes on PRs as:
    /// the hosts' review tokens, with no other login to fall back on.
    pub fn review_account(&self) -> Account {
        let forges = &self.config.current().machine.forges;
        let identity = format!("review {}", forges.identity(Whose::Review));
        Account::user(forges.logins(Whose::Review))
            .remembering(self.memories.of(&identity))
            .naming_missing(Whose::Review.missing())
    }
}
