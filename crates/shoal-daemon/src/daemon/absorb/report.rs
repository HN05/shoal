//! What an absorb tells: each side's log, and the agent of the space that
//! took the work, when another caller absorbed it there.
use anyhow::Result;

use crate::daemon::{
    events::{self, EventKind},
    log,
    manager::Manager,
    scope::Caller,
};
use shoal_protocol::{
    absorb::Absorbed,
    events::AbsorbEvent,
    model::{LinkedItem, SpaceLinks},
};

impl Manager {
    /// Journal `absorbed` in both spaces' logs, and tell the agent of the
    /// space that took the work what it took unless it is the `caller`.
    pub(super) async fn report_absorb(&self, absorbed: &Absorbed, caller: Option<&Caller>) {
        if let Err(error) = self.journal_absorb(absorbed).await {
            log!("absorb of {} not journaled: {error:#}", absorbed.space.id);
        }
        if caller.is_some_and(|caller| caller.space_id == absorbed.into.id) {
            return;
        }
        let message = taken_over(absorbed);
        let kind = crate::daemon::alerts::AlertKind::SpaceAbsorbed;
        let queued = self.queue_agent_message(&absorbed.into.id, kind, message);
        if let Err(error) = queued.await {
            log!("absorb of {} not told: {error:#}", absorbed.space.id);
        }
    }

    /// Record that the absorbed space went into the space that took it, by
    /// which merge method, and what that space took over.
    async fn journal_absorb(&self, absorbed: &Absorbed) -> Result<()> {
        let method = absorbed.merge.as_ref().map(|merge| merge.method);
        let into = AbsorbEvent {
            space: absorbed.into.clone(),
            method,
            links: Vec::new(),
            files: Vec::new(),
            threads: Vec::new(),
            children: Vec::new(),
            carried: Vec::new(),
        };
        let took = AbsorbEvent {
            space: absorbed.space.clone(),
            method,
            links: link_urls(&absorbed.links),
            files: absorbed.files.clone(),
            threads: absorbed.threads.clone(),
            children: absorbed.children.clone(),
            carried: absorbed.carried.clone(),
        };
        let (target, taker) = (absorbed.space.id.clone(), absorbed.into.id.clone());
        (self.store)
            .write(move |tx| {
                events::record_absorb(tx, &target, EventKind::Absorbed, &into)?;
                events::record_absorb(tx, &taker, EventKind::AbsorbedSpace, &took)
            })
            .await
    }
}

/// The URLs of every link `links` lists.
fn link_urls(links: &SpaceLinks) -> Vec<String> {
    let lists = [&links.issues, &links.prs, &links.opened, &links.related];
    (lists.into_iter().flatten())
        .map(|item: &LinkedItem| item.url.to_string())
        .collect()
}

/// What the agent of the space that took `absorbed`'s work is told: what
/// merged and what it took over, one sentence for each.
fn taken_over(absorbed: &Absorbed) -> String {
    let mut sentences = vec![format!(
        "Shoal absorbed {} into this space.",
        absorbed.space.name
    )];
    if let Some(merge) = &absorbed.merge {
        let commits = match merge.commits.is_empty() {
            true => String::new(),
            false => format!(": {}", merge.commits.join("; ")),
        };
        sentences.push(format!(
            "Its branch {} merged into {} by {}{commits}.",
            merge.branch, merge.into, merge.method
        ));
    }
    if absorbed.discarded {
        sentences.push("Its code stayed behind on its branch and in its checkpoint.".to_owned());
    }
    let listed = [
        (
            "Its uncommitted files are uncommitted changes here, to review and commit",
            absorbed.carried.clone(),
        ),
        ("Its links are this space's now", link_urls(&absorbed.links)),
        (
            "Its attached files are this space's now",
            absorbed.files.clone(),
        ),
        (
            "Its open threads are this space's now",
            (absorbed.threads.iter()).map(ToString::to_string).collect(),
        ),
        (
            "The spaces below it moved up below its parent",
            (absorbed.children.iter())
                .map(|child| child.name.to_string())
                .collect(),
        ),
    ];
    for (what, items) in listed {
        if !items.is_empty() {
            sentences.push(format!("{what}: {}.", items.join(", ")));
        }
    }
    sentences.join(" ")
}
