//! Which linked PRs a ready mark may cover: open ones, at the head the forge
//! reports for them, which a workspace's PR must share with its HEAD.
use std::collections::HashMap;

use anyhow::{Context, Result};

use super::super::Manager;
use crate::{
    daemon::links::{space::LinkSpace, wait::linked_items},
    forge::items::{item::unmarkable_reason, link::ItemKind},
    model::SkippedPr,
};

/// The linked PRs a mark covers, sorted by whether they can be marked.
#[derive(Default)]
pub(super) struct Pulls {
    /// The forge's head of each PR that can be marked.
    pub(super) heads: HashMap<String, String>,
    pub(super) skipped: Vec<SkippedPr>,
}

impl Pulls {
    pub(super) fn skip_reason(&self, url: &str) -> Option<&str> {
        (self.skipped.iter())
            .find(|skipped| skipped.url == url)
            .map(|skipped| skipped.reason.as_str())
    }
}

impl Manager {
    /// Read each linked PR the marks of `kind`, or the `explicit` item,
    /// cover from the forge, and skip those that are not open or, given the
    /// workspace's `head`, whose head on the forge is another commit.
    pub(super) async fn markable_pulls(
        &self,
        space: &LinkSpace,
        kind: Option<ItemKind>,
        explicit: Option<&(String, ItemKind)>,
        head: Option<&str>,
    ) -> Result<Pulls> {
        let urls = self.covered_pulls(space, kind, explicit).await?;
        let mut pulls = Pulls::default();
        if urls.is_empty() {
            return Ok(pulls);
        }
        let forge = self.space_forge(space).await?;
        let account = self.agent_account();
        for url in urls {
            let (number, _) = forge.pull(&url)?;
            let pull = (forge.read(&account, ItemKind::Pr, number).await)
                .with_context(|| format!("could not read {url}"))?;
            let state = pull.item.state;
            match unmarkable_reason(state, pull.head_sha.as_deref(), head) {
                Some(reason) => pulls.skipped.push(SkippedPr { url, reason }),
                None => {
                    let head = pull.head_sha.context("PR has no head commit")?;
                    pulls.heads.insert(url, head);
                }
            }
        }
        Ok(pulls)
    }

    /// The linked PRs the marks of `kind`, or the `explicit` item, cover; an
    /// unlinked one is left for marking to refuse.
    async fn covered_pulls(
        &self,
        space: &LinkSpace,
        kind: Option<ItemKind>,
        explicit: Option<&(String, ItemKind)>,
    ) -> Result<Vec<String>> {
        let selected = explicit.map_or(kind, |(_, kind)| Some(*kind));
        if selected == Some(ItemKind::Issue) {
            return Ok(Vec::new());
        }
        let id = space.id().to_owned();
        let linked = self
            .store
            .run(move |db| linked_items(db, &id, Some(ItemKind::Pr)));
        let mut urls: Vec<String> = linked.await?.into_iter().map(|(url, _)| url).collect();
        if let Some((url, _)) = explicit {
            urls.retain(|linked| linked == url);
        }
        Ok(urls)
    }
}
