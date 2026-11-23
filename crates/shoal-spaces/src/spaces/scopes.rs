//! The scope tokens a space issues on request, which act for it until
//! revoked or the space is removed, for processes Shoal does not track,
//! such as a shell given its environment or a chat a scoped process runs.
use crate::Spaces;
use crate::scope::tokens;
use anyhow::{Result, ensure};
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

impl Spaces {
    /// Issue a new scope token that acts for the space `selector` names.
    pub async fn issue_scope(&self, selector: &SpaceSelector) -> Result<ScopeToken> {
        let id = self.space(selector).await?.id;
        self.store.write(move |tx| tokens::issue(tx, &id)).await
    }

    /// Revoke `token`, which the space `selector` names issued.
    pub async fn revoke_scope(&self, selector: &SpaceSelector, token: ScopeToken) -> Result<()> {
        let id = self.space(selector).await?.id;
        let revoked = (self.store)
            .write(move |tx| tokens::revoke(tx, &id, &token))
            .await?;
        ensure!(revoked, "space {selector} issued no such scope token");
        Ok(())
    }
}
