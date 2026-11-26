//! The scope tokens a space issues on request, for processes Shoal does not
//! track, such as a shell given its environment. They act for the space
//! until revoked or the space is removed.
use crate::Spaces;
use crate::scope::tokens;
use anyhow::{Result, ensure};
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

impl Spaces {
    pub async fn issue_scope(&self, selector: &SpaceSelector) -> Result<ScopeToken> {
        let id = self.space(selector).await?.id;
        self.store.write(move |tx| tokens::issue(tx, &id)).await
    }

    pub async fn revoke_scope(&self, selector: &SpaceSelector, token: ScopeToken) -> Result<()> {
        let id = self.space(selector).await?.id;
        let revoked = (self.store)
            .write(move |tx| tokens::revoke(tx, &id, &token))
            .await?;
        ensure!(revoked, "space {selector} issued no such scope token");
        Ok(())
    }
}
