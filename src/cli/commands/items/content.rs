//! What an edit or comment carries, read from its arguments: text, bodies
//! from files, and the files to attach.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::args::{
        BodyArgs,
        changes::{AttachArgs, EditArgs, PrEditArgs},
    },
    forge::items::{
        action::{Action, Edit, MilestoneChange},
        attach::{self, Attachment},
        comment::CommentEdit,
    },
};

/// The edit of a PR, with the changes only PRs take.
pub(super) fn pr_edit(edit: PrEditArgs) -> Result<Edit> {
    Ok(Edit {
        base: edit.base,
        add_reviewers: edit.add_reviewers,
        remove_reviewers: edit.remove_reviewers,
        draft: (edit.draft || edit.no_draft).then_some(edit.draft),
        ..edit_fields(edit.edit)?
    })
}

pub(super) fn edit_fields(edit: EditArgs) -> Result<Edit> {
    let (body, attachments) = read_content(edit.body, edit.files)?;
    Ok(Edit {
        title: edit.title,
        body,
        add_labels: edit.add_labels,
        remove_labels: edit.remove_labels,
        add_assignees: edit.add_assignees,
        remove_assignees: edit.remove_assignees,
        milestone: match (edit.milestone, edit.clear_milestone) {
            (Some(title), _) => Some(MilestoneChange::Set(title)),
            (None, true) => Some(MilestoneChange::Clear),
            (None, false) => None,
        },
        attachments,
        ..Edit::default()
    })
}

/// A new comment, reported as `posted`, or the edit of comment `edit`.
pub(super) fn comment(
    body: BodyArgs,
    files: AttachArgs,
    edit: Option<u64>,
    posted: &'static str,
) -> Result<(Action, &'static str)> {
    let (body, attachments) = read_content(body, files)?;
    if let Some(comment) = edit {
        let body = body.context("editing a comment needs --body or --body-file")?;
        return Ok((
            Action::EditComment(CommentEdit { comment, body }),
            "Comment edited",
        ));
    }
    ensure!(
        body.is_some() || !attachments.is_empty(),
        "a comment needs --body, --body-file or --attach"
    );
    let action = Action::Comment {
        body: body.unwrap_or_default(),
        attachments,
    };
    Ok((action, posted))
}

/// The body and the files to attach, checked before anything is sent, with
/// the body's references to them pointing at their paths.
pub(super) fn read_content(
    body: BodyArgs,
    files: AttachArgs,
) -> Result<(Option<String>, Vec<Attachment>)> {
    // Only attachments resolve paths, which a deleted working directory cannot.
    if files.attach.is_empty() {
        return Ok((read_body(body)?, Vec::new()));
    }
    let cwd = std::env::current_dir().context("read the working directory")?;
    let attachments: Vec<_> = files
        .attach
        .iter()
        .map(|arg| Attachment::parse(arg, &cwd))
        .collect();
    attach::check_all(&attachments)?;
    let body_dir = body
        .body_file
        .as_ref()
        .and_then(|path| cwd.join(path).parent().map(Path::to_path_buf));
    let text = read_body(body)?.map(|text| {
        let dirs: Vec<&Path> = body_dir.as_deref().into_iter().chain([&*cwd]).collect();
        attach::resolve_references(&text, &attachments, &dirs)
    });
    Ok((text, attachments))
}

pub(crate) fn read_body(body: BodyArgs) -> Result<Option<String>> {
    match body.body_file {
        Some(path) => std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))
            .map(Some),
        None => Ok(body.body),
    }
}
