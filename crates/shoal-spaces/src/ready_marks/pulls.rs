//! Which linked PRs a ready mark may cover: open ones, whose head on the
//! forge a workspace's PR must share with its HEAD.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::forge::items::reference::ItemRef;
use crate::model::Space;

use anyhow::{Context, Result};
use shoal_refs::CommitId;

use crate::{
    forge::items::{item::unmarkable_reason, link::ItemKind},
    model::SkippedPr,
};
use shoal_forge::items::action::ActionRequests;

/// The linked PRs a mark covers that cannot be marked.
#[derive(Default)]
pub(super) struct Pulls {
    pub(super) skipped: Vec<SkippedPr>,
}

impl Pulls {
    pub(super) fn skip_reason(&self, url: &ItemRef) -> Option<&str> {
        (self.skipped.iter())
            .find(|skipped| skipped.url == *url)
            .map(|skipped| skipped.reason.as_str())
    }
}

impl Spaces {
    /// Read each linked PR the marks of `kind`, or the `explicit` item,
    /// cover from the forge, and skip those that are not open or, given the
    /// workspace's `head`, whose head on the forge is another commit.
    pub(super) async fn markable_pulls(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        kind: Option<ItemKind>,
        explicit: Option<&ItemRef>,
        head: Option<&str>,
    ) -> Result<Pulls> {
        let urls = self.covered_pulls(space, kind, explicit).await?;
        let mut pulls = Pulls::default();
        if urls.is_empty() {
            return Ok(pulls);
        }
        let forge = daemon.space_forge(space).await?;
        let account = daemon.agent_account();
        for url in urls {
            url.belongs_to(&forge)?;
            let pull = (forge.read(&account, ItemKind::Pr, url.pull()?.get()).await)
                .with_context(|| format!("could not read {url}"))?;
            let state = pull.item.state;
            let pull_head = pull.head_sha.as_ref().map(CommitId::as_str);
            if let Some(reason) = unmarkable_reason(state, pull_head, head) {
                pulls.skipped.push(SkippedPr { url, reason });
            }
        }
        Ok(pulls)
    }

    /// The linked PRs the marks of `kind`, or the `explicit` item, cover; an
    /// unlinked one is left for marking to refuse.
    async fn covered_pulls(
        &self,
        space: &Space,
        kind: Option<ItemKind>,
        explicit: Option<&ItemRef>,
    ) -> Result<Vec<ItemRef>> {
        let selected = explicit.map_or(kind, |item| item.kind().item());
        if selected == Some(ItemKind::Issue) {
            return Ok(Vec::new());
        }
        let (id, records) = (space.id.to_owned(), self.records.clone());
        let linked = self
            .store
            .run(move |db| records.assigned_items(db, &id, Some(ItemKind::Pr)));
        let mut urls = linked.await?;
        if let Some(url) = explicit {
            urls.retain(|linked| linked == url);
        }
        Ok(urls)
    }
}
