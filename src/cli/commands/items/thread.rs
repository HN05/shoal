//! `shoal pr reply`, `resolve` and `unresolve`: changes to a review thread.
use anyhow::{Context as _, Result, ensure};

use super::{content::read_body, select_prs};
use crate::{
    cli::{
        args::{BodyArgs, changes::ItemSelector},
        context::Context,
        ui,
    },
    forge::{
        ForgeRepo,
        capability::Capability,
        items::action::{Action, ThreadChange},
    },
};

/// A reply in the thread of `comment`, which needs a body.
pub(super) fn reply(comment: u64, body: BodyArgs) -> Result<Action> {
    let reply = read_body(body)?.context("a reply needs --body or --body-file")?;
    Ok(thread(comment, Some(reply), None))
}

/// Resolving the thread of `comment`, after replying with the body given.
/// Without one, it first asks unless `confirmed`.
pub(super) async fn resolve(
    ctx: &Context,
    item: &ItemSelector,
    comment: u64,
    body: BodyArgs,
    confirmed: bool,
) -> Result<Action> {
    let reply = read_body(body)?;
    if reply.is_none() && !confirmed {
        confirm_unanswered(ctx, item, comment).await?;
    }
    Ok(thread(comment, reply, Some(true)))
}

pub(super) fn thread(comment: u64, reply: Option<String>, resolve: Option<bool>) -> Action {
    Action::Thread(ThreadChange {
        comment,
        reply,
        resolve,
    })
}

/// Ask before resolving a thread without a reply, which leaves the reviewer
/// no reason. Without exactly one selected PR the action itself fails, so
/// there is nothing to ask.
async fn confirm_unanswered(ctx: &Context, selector: &ItemSelector, comment: u64) -> Result<()> {
    let selected = select_prs(ctx, selector).await?;
    let [item] = selected.items.as_slice() else {
        return Ok(());
    };
    let forge = ForgeRepo::resolve_pull_url(&item.url, &ctx.forge_hosts()?).await?;
    forge.require(Capability::ResolveThreads)?;
    let (number, _) = forge.pull(&item.url)?;
    let thread = ctx
        .progress(
            "Reading thread",
            forge.thread(&ctx.forge_account()?, number, comment),
        )
        .await?;
    if thread.replies == 0 {
        ensure!(
            ui::confirm(
                ctx,
                &format!("Thread {} has no reply.", thread.first),
                "--body-file with the reason, or --yes"
            )?,
            "thread resolution canceled"
        );
    }
    Ok(())
}
