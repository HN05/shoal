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
        Box::pin(self.spaces.prepare_merge(self, selector, options, caller))
    }

    fn prepare_unmerge<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<UnmergePlan>> {
        Box::pin(self.spaces.prepare_unmerge(self, selector))
    }

    fn authorize_scoped_merge<'a>(
        &'a self,
        plan: &'a MergePlan,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.authorize_scoped_merge(self, plan, caller))
    }

    fn authorize_scoped_unmerge<'a>(
        &'a self,
        plan: &'a UnmergePlan,
        caller: &'a Caller,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.spaces.authorize_scoped_unmerge(self, plan, caller))
    }

    fn update_merge_records<'a>(&'a self, change: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(self.spaces.update_merge_records(change))
    }

    fn follow_pushed_merge_runs<'a>(&'a self, change: &'a RecordChange) -> BoxFuture<'a, ()> {
        Box::pin(self.spaces.follow_pushed_merge_runs(self, change))
    }
}
