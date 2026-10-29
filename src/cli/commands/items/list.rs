//! `shoal pr list` and `shoal issue list`: search the items of the
//! repository of the workspace or registered checkout.
use anyhow::Result;

use super::context::select_context;

use crate::{
    cli::{args::changes::ListArgs, client, context::Context},
    forge::items::{
        item::{Item, ItemState},
        link::ItemKind,
        list::Search,
    },
    protocol::ItemMethod,
};

pub(super) async fn list(ctx: &Context, kind: ItemKind, args: ListArgs) -> Result<i32> {
    let query = args.query.join(" ");
    let search = Search {
        state: args.state,
        labels: args.labels,
        author: args.author,
        text: (!query.is_empty()).then_some(query),
        limit: args.limit.into(),
    };
    // Refuse a bad filter before the workspace picker asks for anything.
    search.check()?;
    let method = ItemMethod::ListItems {
        context: select_context(ctx, args.workspace).await?,
        kind,
        search,
    };
    let items = ctx
        .progress("Searching", client::pages::<Item>(&ctx.paths, method))
        .await?;
    ctx.show(&items, |items| render_list(kind, items))?;
    Ok(0)
}

/// One line per item: its number, state, title and labels.
fn render_list(kind: ItemKind, items: &[Item]) {
    if items.is_empty() {
        match kind {
            ItemKind::Issue => outln!("No issues"),
            ItemKind::Pr => outln!("No PRs"),
        }
        return;
    }
    let width = items
        .iter()
        .map(|item| item.number.to_string().len())
        .max()
        .unwrap_or(0);
    for item in items {
        let state = match &item.pr {
            Some(pr) if pr.draft && item.state == ItemState::Open => "draft",
            _ => item.state.as_str(),
        };
        let mut line = format!("#{:<width$}  {state:<6}  {}", item.number, item.title);
        if !item.labels.is_empty() {
            line += &format!("  [{}]", item.labels.join(", "));
        }
        outln!("{line}");
    }
}
