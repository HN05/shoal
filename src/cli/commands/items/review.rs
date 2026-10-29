//! `shoal pr review`: a review from its verdict, body and line comments,
//! given as arguments or a JSON array.
use std::{io::Read as _, path::Path};

use anyhow::{Context as _, Result, bail};

use super::read_body;
use crate::{
    cli::args::changes::{ReviewArgs, VerdictArgs},
    forge::{
        items::action::Action,
        review::submit::{LineComment, NewReview, ReviewVerdict, Side},
    },
};

/// The review `args` describe, and what to report once it is submitted.
pub(super) fn review(args: ReviewArgs) -> Result<(Action, &'static str)> {
    let verdict = verdict(&args.verdict);
    let mut comments = line_comments(&args.lines, Side::New)?;
    comments.extend(line_comments(&args.old_lines, Side::Old)?);
    if let Some(path) = &args.comments_file {
        comments.extend(comments_file(path)?);
    }
    let review = NewReview {
        verdict,
        body: read_body(args.body)?.unwrap_or_default(),
        comments,
    };
    let outcome = match verdict {
        ReviewVerdict::Comment => "Review posted",
        ReviewVerdict::Approve => "PR approved",
        ReviewVerdict::RequestChanges => "Changes requested",
    };
    Ok((Action::Review(review), outcome))
}

/// The verdict clap allows exactly one flag of.
fn verdict(flags: &VerdictArgs) -> ReviewVerdict {
    if flags.approve {
        ReviewVerdict::Approve
    } else if flags.request_changes {
        ReviewVerdict::RequestChanges
    } else {
        ReviewVerdict::Comment
    }
}

/// The comments of `PATH:LINE BODY` argument pairs on `side`.
fn line_comments(pairs: &[String], side: Side) -> Result<Vec<LineComment>> {
    let (pairs, _) = pairs.as_chunks::<2>();
    pairs
        .iter()
        .map(|[place, body]| {
            let Some((path, line)) = place.rsplit_once(':') else {
                bail!("{place} names no line; give PATH:LINE");
            };
            let line = line
                .parse()
                .with_context(|| format!("{place} names no line; give PATH:LINE"))?;
            Ok(LineComment {
                path: path.to_owned(),
                line,
                side,
                body: body.clone(),
            })
        })
        .collect()
}

/// The comments of the JSON array in the file at `path`, or on standard
/// input for `-`.
fn comments_file(path: &Path) -> Result<Vec<LineComment>> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("read line comments from standard input")?;
        text
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    };
    serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not a JSON array of line comments with path, line, body and an optional side of new or old",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests;
