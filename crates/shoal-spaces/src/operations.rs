//! Starting the daemon's operations that work in a workspace, such as its
//! preparation, a rename or a merge, in a worker of their own.
use std::collections::BTreeMap;

use anyhow::Result;
use shoal_daemon_base::callers::Caller;
use shoal_protocol::{protocol::operations::OperationSpec, space_names::SpaceSelector};

use crate::Spaces;

impl Spaces {
    /// The worker acts with `caller`'s scope. The scope check already
    /// narrowed a scoped caller's workspace to an ID. A move starts with its
    /// own request.
    pub async fn start_operation(
        &self,
        mut spec: OperationSpec,
        env: Option<BTreeMap<String, String>>,
        caller: Option<&Caller>,
    ) -> Result<String> {
        let unmerge = matches!(spec, OperationSpec::Unmerge { .. });
        let selector = (spec.workspace_mut())
            .ok_or_else(|| anyhow::anyhow!("a move starts with its own request"))?;
        let workspace = match unmerge {
            true => self.unmerge_workspace(selector).await?,
            false => self.workspace(selector).await?,
        }
        .id;
        *selector = SpaceSelector::id(&workspace);
        self.operations.spawn(workspace, spec, env, caller).await
    }
}
