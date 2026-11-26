//! `shoal absorb`.
use anyhow::Result;

use crate::cli::{
    args::lifecycle::AbsorbArgs,
    context::Context,
    output::{Palette, Style},
};
use shoal_protocol::absorb::{Absorb, Absorbed};

pub(super) async fn run(ctx: &Context, args: AbsorbArgs) -> Result<i32> {
    let request = Absorb {
        space: args.space,
        into: args.into,
        method: args.method,
        message: args.message,
        discard_uncommitted: args.discard_uncommitted,
        force: args.force,
    };
    let absorbed = crate::client::removal::absorb(&ctx.paths, request).await?;
    super::links::print_warnings(&absorbed.warnings);
    let palette = Palette::stdout(ctx.json);
    ctx.emit(&report(&absorbed, &palette), &absorbed)?;
    Ok(0)
}

/// What the absorb did, a line for each kind of thing that moved.
fn report(absorbed: &Absorbed, palette: &Palette) -> String {
    let (space, into) = (&absorbed.space.name, &absorbed.into.name);
    let mut lines = vec![format!(
        "Absorbed {} into {}; its removal goes on in the background",
        palette.paint(Style::Heading, space),
        palette.paint(Style::Heading, into),
    )];
    if let Some(merge) = &absorbed.merge {
        let commits = match merge.commits.len() {
            1 => "1 commit".to_owned(),
            count => format!("{count} commits"),
        };
        lines.push(format!(
            "Merged {} ({commits}) into {} by {}",
            merge.branch, merge.into, merge.method
        ));
    }
    if absorbed.discarded {
        lines.push("Left its code behind; its branch and checkpoint keep it".to_owned());
    }
    let links = (absorbed.links.issues.iter())
        .chain(&absorbed.links.prs)
        .chain(&absorbed.links.opened)
        .chain(&absorbed.links.related)
        .map(|item| item.url.to_string());
    let listed = [
        ("Uncommitted files carried over", absorbed.carried.clone()),
        ("Links moved", links.collect()),
        ("Files moved", absorbed.files.clone()),
        (
            "Threads moved",
            (absorbed.threads.iter()).map(ToString::to_string).collect(),
        ),
        (
            "Moved up a level",
            (absorbed.children.iter())
                .map(|child| child.name.to_string())
                .collect(),
        ),
        (
            "Requests withdrawn",
            (absorbed.withdrawn.iter())
                .map(ToString::to_string)
                .collect(),
        ),
    ];
    for (what, items) in listed {
        if !items.is_empty() {
            lines.push(format!("{what}: {}", items.join(", ")));
        }
    }
    lines.join("\n")
}
