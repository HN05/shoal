//! The merges and unmerges an execution runs.
use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::daemon::Merges;

use crate::daemon::manager::Manager;
use crate::daemon::scope::Caller;
use crate::model::MergeOptions;
use crate::model::MergePlan;
use crate::model::UnmergePlan;
use shoal_agents::execution_connection::RecordChange;
use shoal_protocol::space_names::SpaceSelector;

impl Merges for Manager {
    fn prepare_merge<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        options: &'a MergeOptions,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<MergePlan>> {
        Box::pin(self.prepare_merge(selector, options, caller))
    }

    fn prepare_unmerge<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<UnmergePlan>> {
        Box::pin(self.prepare_unmerge(selector))
    }

    fn authorize_scoped_merge<'a>(
        &'a self,
        plan: &'a MergePlan,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.authorize_scoped_merge(plan, caller))
    }

    fn authorize_scoped_unmerge<'a>(
        &'a self,
        plan: &'a UnmergePlan,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.authorize_scoped_unmerge(plan, caller))
    }

    fn update_merge_records<'a>(&'a self, change: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(self.update_merge_records(change))
    }

    fn follow_pushed_merge_runs<'a>(&'a self, change: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(self.follow_pushed_merge_runs(change))
    }
}
