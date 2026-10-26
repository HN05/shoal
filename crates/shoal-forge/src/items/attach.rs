//! Files attached to a description or comment: the CLI checks them and points
//! the body's references at them, and the daemon uploads them and shows each
//! where the body references it, or after the body.
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, account::Account, backends::Backend, capability::Capability};

/// The largest image Shoal uploads, which is GitHub's limit.
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// The largest video or other file Shoal uploads. Forges limit these by plan
/// or configuration, so this generous bound leaves the rest to them.
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// The content type of files whose extension [`TYPES`] does not name.
const OTHER_TYPE: &str = "application/octet-stream";

/// Extensions and their content types. The forge decides which files it
/// accepts; images and videos are shown, other files linked.
const TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("mp4", "video/mp4"),
    ("mov", "video/quicktime"),
    ("webm", "video/webm"),
    ("txt", "text/plain"),
    ("log", "text/plain"),
    ("md", "text/markdown"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("patch", "text/x-diff"),
    ("json", "application/json"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tgz", "application/gzip"),
];

/// A file to attach, by absolute path, with an image's alt text or a
/// file's link text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
    #[serde(default)]
    pub alt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Media {
    Image,
    Video,
    /// Any other file, which bodies link to.
    File,
}

impl Attachment {
    /// `<path>[#<alt text>]`, with the path relative to `cwd`.
    pub fn parse(arg: &str, cwd: &Path) -> Self {
        let (path, alt) = match arg.split_once('#') {
            Some((path, alt)) => (path, Some(alt.trim()).filter(|alt| !alt.is_empty())),
            None => (arg, None),
        };
        Self {
            path: cwd.join(path),
            alt: alt.map(str::to_owned),
        }
    }

    pub fn name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment")
    }

    pub(crate) fn content_type(&self) -> &'static str {
        let extension = self
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        TYPES
            .iter()
            .find(|(known, _)| *known == extension)
            .map_or(OTHER_TYPE, |(_, content_type)| *content_type)
    }

    pub(crate) fn media(&self) -> Media {
        let content_type = self.content_type();
        if content_type.starts_with("image/") {
            Media::Image
        } else if content_type.starts_with("video/") {
            Media::Video
        } else {
            Media::File
        }
    }

    /// Refuse what no upload would accept: a relative path, a missing or
    /// empty file, or a file over its kind's limit.
    pub fn check(&self) -> Result<()> {
        let path = self.path.display();
        ensure!(self.path.is_absolute(), "{path}: give an absolute path");
        let metadata = std::fs::metadata(&self.path).with_context(|| format!("read {path}"))?;
        ensure!(metadata.is_file(), "{path} is not a file");
        ensure!(metadata.len() > 0, "{path} is empty");
        let (limit, kind) = match self.media() {
            Media::Image => (MAX_IMAGE_BYTES, "images"),
            Media::Video => (MAX_FILE_BYTES, "videos"),
            Media::File => (MAX_FILE_BYTES, "files"),
        };
        ensure!(
            metadata.len() <= limit,
            "{path}: {kind} must be at most {} MiB",
            limit / 1024 / 1024
        );
        Ok(())
    }
}

/// Check every attachment and refuse attaching one file twice.
pub fn check_all(attachments: &[Attachment]) -> Result<()> {
    for (index, attachment) in attachments.iter().enumerate() {
        attachment.check()?;
        if attachments[..index]
            .iter()
            .any(|other| other.path == attachment.path)
        {
            bail!("{} is attached twice", attachment.path.display());
        }
    }
    Ok(())
}

/// Point each image reference in `body` whose target names an attachment,
/// relative to the first of `dirs` where it exists, at the attachment's path.
pub fn resolve_references(body: &str, attachments: &[Attachment], dirs: &[&Path]) -> String {
    let attached = |target: &str| {
        let candidate = dirs
            .iter()
            .map(|dir| dir.join(target))
            .find(|candidate| candidate.exists())?;
        let candidate = candidate.canonicalize().ok()?;
        attachments
            .iter()
            .find(|attachment| attachment.path.canonicalize().ok() == Some(candidate.clone()))
    };
    replace_images(body, |image| {
        let attachment = attached(image.target)?;
        Some(format!("![{}](<{}>)", image.alt, attachment.path.display()))
    })
}

/// Point the body's references to each of `attachments` at the file in
/// `copies` at the same position.
pub fn retarget(body: &str, attachments: &[Attachment], copies: &[Attachment]) -> String {
    replace_images(body, |image| {
        let index = attachments
            .iter()
            .position(|attachment| Path::new(image.target) == attachment.path)?;
        Some(format!(
            "![{}](<{}>)",
            image.alt,
            copies[index].path.display()
        ))
    })
}

/// Replace each image reference to an attachment with `embed(index, alt)`, preferring the reference's alt text, and add the attachments the
/// body does not reference after it.
pub(crate) fn embed(
    body: &str,
    attachments: &[Attachment],
    embed: impl Fn(usize, &str) -> String,
) -> String {
    let mut referenced = vec![false; attachments.len()];
    let mut body = replace_images(body, |image| {
        let index = attachments
            .iter()
            .position(|attachment| Path::new(image.target) == attachment.path)?;
        referenced[index] = true;
        let alt = Some(image.alt)
            .filter(|alt| !alt.trim().is_empty())
            .or(attachments[index].alt.as_deref())
            .unwrap_or(attachments[index].name());
        Some(embed(index, alt))
    });
    for (index, attachment) in attachments.iter().enumerate() {
        if referenced[index] {
            continue;
        }
        let alt = attachment.alt.as_deref().unwrap_or(attachment.name());
        if !body.trim().is_empty() {
            body = format!("{}\n\n", body.trim_end());
        }
        body.push_str(&embed(index, alt));
    }
    body
}

/// What an uploaded file belongs to on the forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Holder {
    Repository,
    /// The issue or PR whose description shows it.
    Item(u64),
    Comment(u64),
}

/// A body to post before its files can be uploaded, and the files.
pub(crate) struct Pending {
    pub body: String,
    pub attachments: Vec<Attachment>,
}

impl ForgeRepo {
    /// Check `attachments`, and that this forge and `account` can upload
    /// them, before anything is posted. Returns whether the forge keeps
    /// uploads with the item or comment that shows them rather than with the
    /// repository.
    pub async fn check_uploads(
        &self,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool> {
        if attachments.is_empty() {
            return Ok(false);
        }
        self.require(Capability::Attachments)?;
        check_all(attachments)?;
        self.kind.check_uploads(self, account, attachments).await
    }

    /// The body to post `body` with, its checked files uploaded to the
    /// repository; where the forge keeps files `per_item` or comment, a body
    /// naming them, and the files to attach once that exists.
    pub(crate) async fn new_body(
        &self,
        account: &Account,
        body: &str,
        attachments: &[Attachment],
        per_item: bool,
    ) -> Result<(String, Option<Pending>)> {
        if attachments.is_empty() {
            return Ok((body.to_owned(), None));
        }
        if !per_item {
            let body = self
                .attach(account, Holder::Repository, body, attachments)
                .await?;
            return Ok((body, None));
        }
        let named = embed(body, attachments, |index, _| {
            format!("`{}`", attachments[index].name())
        });
        let pending = Pending {
            body: body.to_owned(),
            attachments: attachments.to_vec(),
        };
        Ok((named, Some(pending)))
    }

    /// Upload the checked `attachments` to `holder` and embed them in `body`.
    pub(crate) async fn attach(
        &self,
        account: &Account,
        holder: Holder,
        body: &str,
        attachments: &[Attachment],
    ) -> Result<String> {
        let repository = self.kind.repository_id(self, account).await?;
        let mut urls = Vec::new();
        for attachment in attachments {
            let request = self.kind.upload(self, holder, attachment, repository)?;
            let response = self
                .send(account, &request)
                .await
                .with_context(|| format!("could not upload {}", attachment.path.display()))?;
            urls.push(self.kind.uploaded_url(&response)?);
        }
        Ok(embed(body, attachments, |index, alt| {
            self.kind.markup(&attachments[index], alt, &urls[index])
        }))
    }
}

/// An inline image, `![alt](target)` or `![alt](<target> "title")`.
struct Image<'a> {
    range: Range<usize>,
    alt: &'a str,
    target: &'a str,
}

/// `body` with each image `replace` returns text for replaced by it.
fn replace_images(body: &str, mut replace: impl FnMut(&Image) -> Option<String>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut copied = 0;
    for image in images(body) {
        if let Some(text) = replace(&image) {
            out.push_str(&body[copied..image.range.start]);
            out.push_str(&text);
            copied = image.range.end;
        }
    }
    out.push_str(&body[copied..]);
    out
}

/// The inline images of `body` outside code, each on one line.
fn images(body: &str) -> Vec<Image<'_>> {
    let code = code_ranges(body);
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = body[from..].find("![").map(|offset| from + offset) {
        from = start + 2;
        if let Some(range) = code.iter().find(|range| range.contains(&start)) {
            from = range.end;
            continue;
        }
        if let Some(image) = image_at(body, start) {
            from = image.range.end;
            found.push(image);
        }
    }
    found
}

/// The byte ranges of `body` that Markdown shows literally: fenced code
/// blocks, and code spans between equal runs of backticks.
fn code_ranges(body: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut fence: Option<(char, usize, usize)> = None;
    let mut prose = 0;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let end = offset + line.len();
        let marker = fence_marker(line);
        match (fence, marker) {
            (None, Some((c, len))) => {
                ranges.extend(code_spans(body, prose..offset));
                fence = Some((c, len, offset));
            }
            (Some((c, len, start)), Some((closing, closing_len)))
                if closing == c
                    && closing_len >= len
                    && line.trim()[closing_len..].trim().is_empty() =>
            {
                ranges.push(start..end);
                fence = None;
                prose = end;
            }
            _ => {}
        }
        offset = end;
    }
    match fence {
        Some((_, _, start)) => ranges.push(start..body.len()),
        None => ranges.extend(code_spans(body, prose..body.len())),
    }
    ranges
}

/// The character and length of the fence a line opens or closes with.
fn fence_marker(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let c = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = trimmed.chars().take_while(|&next| next == c).count();
    (len >= 3).then_some((c, len))
}

/// The code spans within `within` of `body`.
fn code_spans(body: &str, within: Range<usize>) -> Vec<Range<usize>> {
    let text = &body[within.clone()];
    let mut spans = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find('`').map(|offset| from + offset) {
        let len = text[start..].chars().take_while(|&c| c == '`').count();
        let mut search = start + len;
        let close = loop {
            let Some(next) = text[search..].find('`').map(|offset| search + offset) else {
                break None;
            };
            let run = text[next..].chars().take_while(|&c| c == '`').count();
            if run == len {
                break Some(next + run);
            }
            search = next + run;
        };
        match close {
            Some(end) => {
                spans.push(within.start + start..within.start + end);
                from = end;
            }
            // An unmatched run is literal backticks.
            None => from = start + len,
        }
    }
    spans
}

fn image_at(body: &str, start: usize) -> Option<Image<'_>> {
    let line_end = body[start..]
        .find('\n')
        .map_or(body.len(), |end| start + end);
    let line = &body[..line_end];
    let alt_start = start + 2;
    let alt_end = alt_start + line[alt_start..].find("](")?;
    let alt = &line[alt_start..alt_end];
    if alt.contains(['[', ']']) {
        return None;
    }
    let target_start = alt_end + 2;
    let (target, after) = match line[target_start..].strip_prefix('<') {
        Some(rest) => {
            let end = rest.find('>')?;
            (&rest[..end], target_start + 1 + end + 1)
        }
        None => {
            let end = line[target_start..]
                .find(|c: char| c == ')' || c.is_whitespace())
                .map_or(line.len(), |end| target_start + end);
            (&line[target_start..end], end)
        }
    };
    let close = after + line[after..].find(')')?;
    // Only a title may come between the target and the closing parenthesis.
    let title = line[after..close].trim();
    if !title.is_empty() && !title.starts_with(['"', '\'', '(']) {
        return None;
    }
    Some(Image {
        range: start..close + 1,
        alt,
        target,
    })
}

#[cfg(test)]
mod tests;
