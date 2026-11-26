//! `shoal thread`.
use crate::cli::ui::Acting;
use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    cli::{
        args::{
            BodyArgs,
            items::RelationArgs,
            threads::{CodeArg, OutcomeData},
        },
        context::Context,
        ui::{self, Fallback},
    },
    client::threads,
};
use shoal_protocol::{
    code::NewCodeRef,
    item_numbers::ItemNumber,
    model::Relation,
    reviews::Severity,
    space_names::SpaceSelector,
    threads::{OutcomeKind, Resolving, Side, ThreadKind, ThreadNumber, ThreadOpening},
};

mod listing;
mod posting;
mod render;
mod switching;

pub(super) use listing::{list, view};
pub(in crate::cli) use posting::{Posting, post, post_in_chat};
pub(in crate::cli) use render::summary;
pub(super) use switching::{Switch, add, remove, switch};

/// The daemon would take a process that cleared its scope token for the user.
fn refuse_workspace_processes(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "workspace processes open, post in and resolve threads only with their scope token"
    );
    Ok(())
}

/// A session Shoal did not start names its own chat without a scope token.
async fn acting_space(ctx: &Context) -> Result<Option<SpaceSelector>> {
    refuse_workspace_processes(ctx)?;
    match super::alerts::in_own_space() {
        true => Ok(Some(super::alerts::own_space(ctx).await?)),
        false => Ok(None),
    }
}

pub(super) struct Opening {
    pub(super) name: String,
    pub(super) code: Vec<CodeArg>,
    pub(super) severity: Option<Severity>,
    pub(super) body: BodyArgs,
}

impl Opening {
    fn read(self) -> Result<ThreadOpening> {
        Ok(ThreadOpening {
            name: self.name,
            body: super::items::read_body(self.body)?,
            code: code_refs(self.code)?,
            severity: self.severity,
        })
    }
}

/// Without a kind, it joins the caller's pending review of that space.
pub(super) async fn open(
    ctx: &Context,
    given: Opening,
    kind: Option<ThreadKind>,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let given = given.read()?;
    let from = acting_space(ctx).await?;
    let to = match space {
        None if from.is_some() => None,
        space => {
            let opening = opening(kind);
            Some(ui::select_space(ctx, space, Fallback::CurrentDirectory(&opening)).await?)
        }
    };
    let thread = threads::open(given, kind, to, from)
        .send(&ctx.paths)
        .await?;
    let number = thread.number;
    let holder = thread.party(Side::Holder);
    let review = (thread.finding.and_then(|finding| finding.review)).filter(|_| thread.held);
    let message = match (review, thread.posts.is_empty()) {
        (Some(review), _) => format!(
            "Thread {number} is a comment of your pending review {review} of {holder}, which it \
             reaches when you submit it with shoal review submit."
        ),
        (None, true) => format!(
            "Thread {number} opened with {holder}; post in it with shoal send {number} \"<text>\"."
        ),
        (None, false) => format!("Thread {number} opened with {holder}, with its opening post."),
    };
    ctx.emit(&message, &thread)?;
    Ok(0)
}

fn code_refs(code: Vec<CodeArg>) -> Result<Vec<NewCodeRef>> {
    let here = std::env::current_dir().context("read the current directory")?;
    Ok((code.into_iter())
        .map(|code| NewCodeRef {
            path: here.join(code.path),
            lines: code.lines,
            contents: None,
        })
        .collect())
}

pub(super) async fn forward(
    ctx: &Context,
    number: ThreadNumber,
    space: Option<SpaceSelector>,
    note: Option<String>,
    unlink: bool,
) -> Result<i32> {
    let from = acting_space(ctx).await?;
    let thread = threads::forward(&ctx.paths, number, space, note, unlink, from).await?;
    let forward = (thread.posts.last()).context("the forward was not recorded")?;
    let side = forward.side.context("a forward comes from a side")?;
    ctx.emit(
        &format!(
            "Thread {} forwarded to {}, who {} it now.",
            thread.number,
            thread.party(side),
            match side {
                Side::Opener => "opens",
                Side::Holder => "holds",
            }
        ),
        &thread,
    )?;
    Ok(0)
}

pub(super) async fn resolve(
    ctx: &Context,
    number: ThreadNumber,
    resolving: Resolving,
) -> Result<i32> {
    let from = acting_space(ctx).await?;
    let thread = threads::resolve(&ctx.paths, number, resolving, from).await?;
    let settled = match &thread.resolution {
        Some(resolution) => resolution.outcome.describe(),
        None => "reopened".into(),
    };
    ctx.emit(&format!("Thread {} {settled}.", thread.number), &thread)?;
    Ok(0)
}

pub(super) fn resolving(
    outcome: Option<OutcomeKind>,
    data: OutcomeData,
    undo: bool,
) -> Result<Resolving> {
    if undo {
        return Ok(Resolving::default());
    }
    if outcome.is_none() {
        bail!("name an outcome, such as answered, done or fixed; or reopen with --undo");
    }
    Ok(Resolving {
        outcome,
        of: data.of,
        reason: data.reason,
        issue: data.issue.map(|issue| issue.to_string()),
    })
}

pub(super) fn selects_thread(items: &RelationArgs) -> bool {
    items.args.first().is_some_and(|word| word == "thread")
}

pub(super) async fn link(ctx: &Context, items: RelationArgs, linked: bool) -> Result<i32> {
    refuse_workspace_processes(ctx)?;
    let number = linked_thread(&items.args, linked)?;
    let space = ui::select_space(ctx, items.space, Fallback::OwnSpace).await?;
    let thread = match linked {
        true => threads::link(&ctx.paths, space, number).await?,
        false => threads::unlink(&ctx.paths, space, number).await?,
    };
    let message = match linked {
        true => format!("Linked thread {number}; its new posts reach this space."),
        false => format!("Unlinked thread {number}."),
    };
    ctx.emit(&message, &thread)?;
    Ok(0)
}

fn linked_thread(args: &[String], linked: bool) -> Result<ItemNumber> {
    let (number, relation) = match args {
        [_, number, relation] => (number, Some(relation)),
        [_, number] if !linked => (number, None),
        _ => bail!("name the thread and its relation: thread <S.N> related"),
    };
    if let Some(relation) = relation {
        let related = <Relation as clap::ValueEnum>::from_str(relation, false).ok()
            == Some(Relation::Related);
        ensure!(related, "a thread is linked as related (got {relation})");
    }
    number.parse()
}

/// Its name and opener decide nothing of which spaces it reaches.
pub(super) fn opening(kind: Option<ThreadKind>) -> Acting {
    Acting::request(move |to| threads::open(ThreadOpening::default(), kind, Some(to), None))
}

#[cfg(test)]
mod tests;
