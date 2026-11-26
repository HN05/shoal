//! The issues, PRs and CI runs the spaces link, through the links service.
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_forge::transport::account::Account;
use shoal_forge_types::activity::status::PrStatus;
use shoal_forge_types::ci::run_action::Run;
use shoal_forge_types::items::action::Action;
use shoal_forge_types::items::issue::IssueDetails;
use shoal_forge_types::items::link::{LinkKind, Selection};
use shoal_forge_types::items::reference::{ItemInput, ItemRef};
use shoal_forge_types::{ForgeRepo, PullRequest};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_links::issue::assignees::AssignerOf;
use shoal_protocol::links::{Unlinked, issue, pr};
use shoal_protocol::model::{
    Assigner, MergePath, MergeWarning, Relation, ReviewMark, Selected, Space, SpaceLinks,
    Workspace, WorkspaceRef,
};
use shoal_protocol::progress::Blocker;
use shoal_protocol::protocol::{ItemContext, RepoTarget};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::status::IssueStatus;
use shoal_refs::CommitId;
use shoal_spaces::daemon::{Dropped, Onto, OpenIssue, PushedMerge, Restack};

use crate::config::Effective;
use crate::daemon::{manager::Manager, scope::Caller};

impl Manager {
    /// The space `space` as a request about its items sees it.
    async fn link_space_of(&self, space: &Space) -> Result<shoal_links::space::LinkSpace> {
        (self.links)
            .link_space(self, &SpaceSelector::id(&space.id))
            .await
    }
}

impl shoal_spaces::daemon::Links for Manager {
    fn current_head<'a>(&'a self, workspace: &'a Workspace) -> BoxFuture<'a, Result<CommitId>> {
        Box::pin(shoal_links::pr::current_head(workspace))
    }

    fn acting_repository<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<RepositoryId>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            Ok(space.repository_id().clone())
        })
    }

    fn origin_path<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<PathBuf>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            Ok(space.origin_path().to_owned())
        })
    }

    fn read_pull_title<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(self.links.read_pull_title(checkout, input))
    }

    fn branch_merge_warning<'a>(
        &'a self,
        selector: &'a SpaceSelector,
    ) -> BoxFuture<'a, Result<Option<MergeWarning>>> {
        Box::pin(Manager::branch_merge_warning(self, selector))
    }

    fn space_forge<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<ForgeRepo>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            shoal_links::forges::of_space(&self.forges, &space).await
        })
    }

    fn linked_states<'a>(
        &'a self,
        remote: &'a Result<Option<String>>,
        pr_cleanup: Option<&'a pr::Registration>,
        issues: &'a [issue::Registration],
    ) -> BoxFuture<'a, (Vec<PrStatus>, Vec<IssueStatus>)> {
        Box::pin(Manager::linked_states(self, remote, pr_cleanup, issues))
    }

    fn acting_settings<'a>(&'a self, space: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            Manager::acting_settings(self, &space).await
        })
    }

    fn settle_agents(&self, id: &SpaceId) {
        self.links.agent_settles().request(id)
    }

    fn apply_own<'a>(
        &'a self,
        id: &'a SpaceId,
        forge: &'a ForgeRepo,
        account: &'a Account,
        item: (shoal_forge_types::items::link::ItemKind, u64, String),
        action: &'a Action,
    ) -> BoxFuture<'a, Result<()>> {
        let (kind, number, url) = item;
        let target = shoal_links::actions::Target { kind, number, url };
        Box::pin(async move {
            (self
                .links
                .apply_own(self, id, forge, account, target, action, None))
            .await?;
            Ok(())
        })
    }

    fn check_conventions<'a>(
        &'a self,
        space: &'a Space,
        settings: &'a Effective,
        marks: &'a mut [ReviewMark],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Ok(space) = self.link_space_of(space).await else {
                return;
            };
            self.links.check_conventions(&space, settings, marks).await
        })
    }

    fn explicit_item<'a>(
        &'a self,
        checkout: &'a Path,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Option<ItemRef>>> {
        Box::pin(self.links.explicit_item(checkout, selection))
    }

    fn follow_pushed_merge(&self, pushed: PushedMerge) -> BoxFuture<'_, Result<()>> {
        let PushedMerge {
            space,
            remotes,
            branch,
            commit,
        } = pushed;
        let pushed = shoal_links::wait::merge_lookups::PushedMerge {
            space,
            remotes,
            branch,
            commit,
        };
        Box::pin(self.links.follow_pushed_merge(self, pushed))
    }

    fn hand_over_assignees<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.links.hand_over_assignees(id))
    }

    fn issue_blockers<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<Blocker>>> {
        Box::pin(self.links.issue_blockers(self, context, item, caller))
    }

    fn issue_details<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        closed: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<IssueDetails>> {
        Box::pin(
            self.links
                .issue_details(self, context, item, closed, caller),
        )
    }

    fn issue_forge<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<(ForgeRepo, ItemRef)>> {
        Box::pin(self.links.issue_forge(checkout, input))
    }

    fn issue_registrations<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Vec<issue::Registration>>> {
        Box::pin(self.links.issue_registrations(id))
    }

    fn lock_change_gates<'a>(
        &'a self,
        id: &'a SpaceId,
        others: &'a [&'a SpaceId],
    ) -> BoxFuture<'a, Result<shoal_daemon_base::gates::PrGuard>> {
        Box::pin(self.links.lock_change_gates(id, others))
    }

    fn awaits_merge_runs<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.links.awaits_merge_runs(id))
    }

    fn merged_unseen<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(self.links.merged_unseen(self, id))
    }

    fn merge_path<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<MergePath>> {
        Box::pin(self.links.merge_path(self, selector))
    }

    fn pr_registration<'a>(
        &'a self,
        id: &'a SpaceId,
    ) -> BoxFuture<'a, Result<Option<pr::Registration>>> {
        Box::pin(self.links.pr_registration(id))
    }

    fn pull_request<'a>(
        &'a self,
        context: &'a ItemContext,
        item: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<PullRequest>> {
        Box::pin(self.links.pull_request(self, context, item, caller))
    }

    fn launch_run<'a>(
        &'a self,
        context: &'a ItemContext,
        run: &'a ItemInput,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Run>> {
        let target = context.clone().into();
        Box::pin(async move { self.links.launch_run(self, &target, run, caller).await })
    }

    fn link_related_run<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        run: &'a ItemRef,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let input = ItemInput::Url(run.clone());
            let (kind, relation) = (LinkKind::Ci, Relation::Related);
            (self.links)
                .link_item(self, selector, kind, &input, relation, None, caller)
                .await
                .map(drop)
        })
    }

    fn read_open_issue<'a>(
        &'a self,
        checkout: &'a Path,
        input: &'a ItemInput,
    ) -> BoxFuture<'a, Result<OpenIssue>> {
        Box::pin(async move {
            let shoal_links::reads::checkouts::OpenIssue { url, title, labels } =
                self.links.read_open_issue(checkout, input).await?;
            Ok(OpenIssue { url, title, labels })
        })
    }

    fn release_assignees(&self, dropped: Vec<Dropped>) -> BoxFuture<'_, Vec<String>> {
        let dropped = (dropped.into_iter())
            .map(|Dropped { url, login }| shoal_links::issue::assignees::Dropped { url, login })
            .collect();
        Box::pin(self.links.release_assignees(dropped))
    }

    fn relink_space<'a>(
        &'a self,
        id: &'a SpaceId,
        links: &'a SpaceLinks,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Vec<String>> {
        Box::pin(self.links.relink_space(self, id, links, caller))
    }

    fn restack<'a>(
        &'a self,
        stacked: &'a WorkspaceRef,
        restack: &'a Restack<'_>,
    ) -> BoxFuture<'a, Result<()>> {
        let restack = shoal_links::stack::Restack {
            base: restack.base,
            head: restack.head,
            merged_heads: restack.merged_heads,
            target: restack.target,
            onto: match restack.onto {
                Onto::Remote => shoal_links::stack::Onto::Remote,
                Onto::Local => shoal_links::stack::Onto::Local,
                Onto::Held => shoal_links::stack::Onto::Held,
            },
            new_base: restack.new_base.clone(),
            cause: restack.cause,
            account: restack.account,
        };
        Box::pin(async move { self.links.restack(self, stacked, &restack).await })
    }

    fn select_context_items<'a>(
        &'a self,
        target: &'a RepoTarget,
        selection: &'a Selection,
        elsewhere: bool,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Selected>> {
        Box::pin(
            self.links
                .select_context_items(self, target, selection, elsewhere, caller),
        )
    }

    fn selected_items<'a>(
        &'a self,
        space: &'a Space,
        selection: &'a Selection,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            self.links.selected_items(&space, selection).await
        })
    }

    fn set_draft<'a>(
        &'a self,
        space: &'a Space,
        urls: &'a [ItemRef],
        draft: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let space = self.link_space_of(space).await?;
            self.links.set_draft(self, &space, urls, draft).await
        })
    }

    fn set_issue<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        input: &'a ItemInput,
        title: Option<String>,
        caller: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(
            self.links
                .set_issue(self, selector, input, title, Assigner::of(caller)),
        )
    }

    fn set_pr<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        action: pr::Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.links.set_pr(self, selector, action))
    }

    fn settle_removed_agents<'a>(
        &'a self,
        space: &'a Space,
        issues: &'a [ItemRef],
    ) -> BoxFuture<'a, Result<Vec<String>>> {
        Box::pin(self.links.settle_removed_agents(self, space, issues))
    }

    fn unlink_items<'a>(
        &'a self,
        selector: &'a SpaceSelector,
        relation: Relation,
        kind: Option<LinkKind>,
        input: Option<&'a ItemInput>,
    ) -> BoxFuture<'a, Result<Unlinked>> {
        Box::pin(
            self.links
                .unlink_items(self, selector, relation, kind, input),
        )
    }
}
