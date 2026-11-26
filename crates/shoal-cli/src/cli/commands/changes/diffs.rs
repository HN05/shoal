//! `shoal diff --thread`.
use anyhow::Result;
use shoal_protocol::{changes::ChangeDiff, protocol::ChangeFilter, threads::ThreadNumber};

use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
    },
    client::changes,
};

pub(in crate::cli::commands) async fn thread(
    ctx: &Context,
    thread: ThreadNumber,
    since: Option<u32>,
) -> Result<i32> {
    let filter = ChangeFilter {
        space: None,
        thread: Some(thread),
        since,
    };
    let diffs = changes::diffs(&ctx.paths, filter).await?;
    ctx.show(&diffs, |diffs| {
        let palette = Palette::stdout(ctx.json);
        for line in lines(diffs, palette) {
            outln!("{line}");
        }
        if diffs.is_empty() {
            outln!("No changes for thread {thread}");
        }
    })?;
    Ok(0)
}

/// `diffs` as a unified diff: a header for each run of changes to one
/// file, then each change's hunk where it is now, or its kind and size for
/// a file without hunks.
fn lines(diffs: &[ChangeDiff], palette: Palette) -> Vec<String> {
    let mut lines = Vec::new();
    let mut file = None;
    for diff in diffs {
        let change = &diff.change;
        let path = (&change.space.id, &change.latest.path);
        if file != Some(path) {
            file = Some(path);
            let heading = format!("{} in {}", change.latest.path, change.space.label());
            lines.push(palette.paint(Style::Heading, heading));
        }
        match diff.patch() {
            Some(patch) => lines.extend(patch.lines().map(|line| painted(line, palette))),
            None => lines.push(format!(
                "change {}: {}, {} bytes",
                change.number,
                change.kind,
                change.size.unwrap_or_default()
            )),
        }
    }
    lines
}

fn painted(line: &str, palette: Palette) -> String {
    let style = match line.as_bytes().first() {
        Some(b'@') => Style::Muted,
        Some(b'+') => Style::Success,
        Some(b'-') => Style::Error,
        _ => return line.to_owned(),
    };
    palette.paint(style, line)
}
