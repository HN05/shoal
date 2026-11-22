//! Files attached to a description or comment: the CLI checks them and points
//! the body's references at them, and the daemon uploads them and shows each
//! where the body references it, or after the body.
use std::{ops::Range, path::Path};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{
    capability::{self, CapabilityRequests, Supports},
    transport::{
        account::Account,
        api::{Request, TransportRequests},
        response::Fetched,
    },
};
use shoal_forge_types::{
    ForgeRepo,
    backends::ForgeKind,
    capability::Capability,
    items::attach::{Attachment, Media},
    numbers::CommentId,
};

/// How the forge uploads, serves and shows attached files.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Attachments: Supports {
    /// The repository's numeric ID, where the forge's uploads name it.
    async fn repository_id(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Refuse `attachments` the forge or `account` cannot upload, before
    /// anything is posted. Returns whether the forge keeps uploads with the
    /// item or comment that shows them rather than with the repository.
    async fn check_uploads(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool>;

    /// The response to a GET of `url`, a file on the forge's web host,
    /// sent as `account`.
    async fn download(&self, _repo: &ForgeRepo, _account: &Account, _url: &str) -> Result<Fetched> {
        Err(capability::unsupported(self, Capability::Downloads))
    }

    /// The request that uploads `attachment` to `holder`.
    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request>;

    /// Where an upload's response says the file is served.
    fn uploaded_url(&self, response: &Value) -> Result<String> {
        served_at(response, "url")
    }

    /// The Markdown that shows an uploaded image or video, or links to
    /// another file.
    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::File => link(alt, url),
            Media::Image | Media::Video => image(alt, url),
        }
    }
}

/// The URL in an upload response's `field`.
pub(crate) fn served_at(response: &Value, field: &str) -> Result<String> {
    response[field]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .context("the forge did not report where it saved the file")
}

/// The Markdown image of `url`.
pub(crate) fn image(alt: &str, url: &str) -> String {
    format!("!{}", link(alt, url))
}

/// The Markdown link to `url`.
pub(crate) fn link(text: &str, url: &str) -> String {
    format!("[{}]({url})", text.replace(['[', ']'], ""))
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
    Comment(CommentId),
}

/// A body to post before its files can be uploaded, and the files.
pub(crate) struct Pending {
    pub body: String,
    pub attachments: Vec<Attachment>,
}

/// Attaching files to items.
#[shoal_forge_derive::extension(pub AttachRequests, pub(crate) AttachSteps)]
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
        self.kind().check_uploads(self, account, attachments).await
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
        let repository = self.kind().repository_id(self, account).await?;
        let mut urls = Vec::new();
        for attachment in attachments {
            let request = self.kind().upload(self, holder, attachment, repository)?;
            let response = self
                .send(account, &request)
                .await
                .with_context(|| format!("could not upload {}", attachment.path.display()))?;
            urls.push(self.kind().uploaded_url(&response)?);
        }
        Ok(embed(body, attachments, |index, alt| {
            self.kind().markup(&attachments[index], alt, &urls[index])
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
