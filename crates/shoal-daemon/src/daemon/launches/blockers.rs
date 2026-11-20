//! What holds a launch's agent: the spaces, queued agents and issues the
//! launch names, and the issues its forge records as blocking the issue
//! the agent works on.
use anyhow::Result;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::progress::Blocker;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    forge::items::{
        link::{LinkKind, Selection},
        reference::ItemInput,
    },
    protocol::ItemContext,
};

impl Manager {
    /// What `named` names in `repository`, an issue by its URL, refused
    /// when the repository has no such workspace, queued agent or issue.
    pub(super) async fn named_blockers(
        &self,
        repository: &RepositoryId,
        named: &[String],
        caller: Option<&Caller>,
    ) -> Result<Vec<Blocker>> {
        if named.is_empty() {
            return Ok(Vec::new());
        }
        let context = self.launch_read_context(repository, caller).await?;
        let mut blockers = Vec::new();
        for input in named {
            blockers.push(match input.parse()? {
                Blocker::Issue(issue) => {
                    Blocker::Issue(self.selected_issue(&context, issue, caller).await?)
                }
                blocker => blocker,
            });
        }
        let selector = repository.into();
        self.check_blockers(&selector, blockers.clone()).await?;
        Ok(blockers)
    }

    /// The issue `input` names in the repository of `context`, by its URL.
    async fn selected_issue(
        &self,
        context: &ItemContext,
        input: ItemInput,
        caller: Option<&Caller>,
    ) -> Result<ItemInput> {
        let selection = Selection {
            kind: Some(LinkKind::Issue),
            input: Some(input),
        };
        let selected = (self.links.select_context_items(
            self,
            &context.clone().into(),
            &selection,
            true,
            caller,
        ))
        .await?;
        let issue = selected.items.into_iter().next();
        Ok(ItemInput::Url(
            anyhow::Context::context(issue, "no issue selected")?.url,
        ))
    }

    /// The open issues the forge records as blocking `issue` of
    /// `repository`. A forge that cannot list them, such as a repository
    /// with blocking issues turned off, leaves them out.
    pub(super) async fn forge_blockers(
        &self,
        repository: &RepositoryId,
        issue: &ItemInput,
        caller: Option<&Caller>,
    ) -> Result<Vec<Blocker>> {
        let context = self.launch_read_context(repository, caller).await?;
        match self
            .links
            .issue_blockers(self, &context, issue, caller)
            .await
        {
            Ok(blockers) => Ok(blockers),
            Err(error) => {
                crate::daemon::log!(
                    "the issues blocking {issue} do not block its agent, since the forge cannot list them: {error:#}"
                );
                Ok(Vec::new())
            }
        }
    }

    /// Where a launch's forge reads in `repository` run for `caller`: in its
    /// own space when that is of the repository, else in the checkout.
    pub(super) async fn launch_read_context(
        &self,
        repository: &RepositoryId,
        caller: Option<&Caller>,
    ) -> Result<ItemContext> {
        let Some(caller) = caller else {
            return Ok(ItemContext::Checkout(repository.clone()));
        };
        // Another machine's space reads through the checkout here.
        let own = self.find_space(&caller.space_id).await?;
        Ok(match own.and_then(|space| space.repository_id) {
            Some(own_repository) if own_repository == *repository => {
                ItemContext::space(SpaceSelector::id(&caller.space_id))
            }
            _ => ItemContext::Checkout(repository.clone()),
        })
    }
}
