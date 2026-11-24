//! The links, watches and merge state that follow an agent's work.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Links;

use crate::daemon::manager::Manager;
use crate::daemon::scope::Caller;
use crate::forge::items::link::LinkKind;
use crate::forge::items::reference::ItemInput;
use crate::model::Relation;
use crate::model::Workspace;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::CommitId;

impl Links for Manager {
    fn settle_agent(&self, id: &SpaceId) {
        self.links.agent_settles().request(id);
    }

    fn acknowledge_item_updates<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        deliveries: Vec<String>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(
            self.links
                .acknowledge_pr_updates(self, selector, deliveries),
        )
    }

    fn agent_updates_recorded<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.agent_updates_recorded(id))
    }

    fn updates_waiting<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.updates_waiting(selector))
    }

    fn restart_watches<'a>(
        &'a self,
        id: &'a SpaceId,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.links.restart_watches(self, id, stop))
    }

    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(shoal_links::pr::current_head(workspace))
    }

    fn link_item<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        kind: LinkKind,
        item: &'a ItemInput,
        relation: Relation,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(async move {
            let linked = (self.links)
                .link_item(self, selector, kind, item, relation, None, caller)
                .await?;
            Ok(linked.warnings)
        })
    }
}
