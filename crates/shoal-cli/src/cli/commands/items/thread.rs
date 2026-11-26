//! `shoal pr reply`, `resolve` and `unresolve`.
use anyhow::{Context as _, Result, ensure};

use super::{content::read_body, item_target};
use crate::{
    cli::{
        args::{BodyArgs, changes::ItemSelector},
        client,
        context::Context,
        ui,
    },
    forge::{
        items::action::{Action, ThreadChange},
        numbers::CommentId,
    },
};

pub(super) fn reply(comment: CommentId, body: BodyArgs) -> Result<Action> {
    let reply = read_body(body)?.context("a reply needs --body or --body-file")?;
    Ok(thread(comment, Some(reply), None))
}

/// Resolving the thread of `comment`, after replying with the body given.
/// Without one, it first asks unless `confirmed`.
pub(super) async fn resolve(
    ctx: &Context,
    item: &ItemSelector,
    comment: CommentId,
    body: BodyArgs,
    confirmed: bool,
) -> Result<Action> {
    let reply = read_body(body)?;
    if reply.is_none() && !confirmed {
        confirm_unanswered(ctx, item, comment).await?;
    }
    Ok(thread(comment, reply, Some(true)))
}

pub(super) fn thread(comment: CommentId, reply: Option<String>, resolve: Option<bool>) -> Action {
    Action::Thread(ThreadChange {
        comment,
        reply,
        resolve,
    })
}

/// Ask before resolving a thread without a reply, which leaves the reviewer
/// no reason.
async fn confirm_unanswered(
    ctx: &Context,
    selector: &ItemSelector,
    comment: CommentId,
) -> Result<()> {
    let reading = {
        let item = selector.item.clone();
        super::in_context(move |target| client::review_thread(target, item.clone(), comment))
    };
    let target = item_target(ctx, selector, &reading).await?;
    let read = client::review_thread(target, selector.item.clone(), comment).send(&ctx.paths);
    let thread = ctx.progress("Reading thread", read).await?;
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
