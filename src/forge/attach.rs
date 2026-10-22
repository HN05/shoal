//! Image and video files attached to a description or comment: the CLI checks
//! them and points the body's references at them, and the daemon uploads them
//! and embeds each where the body references it, or after the body.
use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ForgeKind, ForgeRepo,
    account::Account,
    api::{Request, Upload, encode},
};

/// The largest image Shoal uploads, which is GitHub's limit.
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// The largest video Shoal uploads. Forges limit videos by plan or
/// configuration, so this generous bound leaves the rest to them.
const MAX_VIDEO_BYTES: u64 = 100 * 1024 * 1024;

/// The extensions Shoal attaches, and their content types.
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
];

/// A file to attach, by absolute path, with the alt text for an image.
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

    pub(crate) fn content_type(&self) -> Result<&'static str> {
        let extension = self
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        TYPES
            .iter()
            .find(|(known, _)| *known == extension)
            .map(|(_, content_type)| *content_type)
            .with_context(|| {
                let known: Vec<_> = TYPES.iter().map(|(extension, _)| *extension).collect();
                format!(
                    "{}: attach an image or video ({})",
                    self.path.display(),
                    known.join(", ")
                )
            })
    }

    pub(crate) fn media(&self) -> Media {
        match self.content_type() {
            Ok(content_type) if content_type.starts_with("video/") => Media::Video,
            _ => Media::Image,
        }
    }

    /// Refuse what no upload would accept: a relative path, a missing or
    /// empty file, another type, or a file over its kind's limit.
    pub fn check(&self) -> Result<()> {
        let path = self.path.display();
        ensure!(self.path.is_absolute(), "{path}: give an absolute path");
        self.content_type()?;
        let metadata = std::fs::metadata(&self.path).with_context(|| format!("read {path}"))?;
        ensure!(metadata.is_file(), "{path} is not a file");
        ensure!(metadata.len() > 0, "{path} is empty");
        let (limit, kind) = match self.media() {
            Media::Image => (MAX_IMAGE_BYTES, "images"),
            Media::Video => (MAX_VIDEO_BYTES, "videos"),
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
    /// Whether this forge keeps uploads with the item or comment that shows
    /// them, rather than with the repository; refuses forges without uploads.
    pub(crate) fn uploads_per_item(&self) -> Result<bool> {
        match self.kind {
            ForgeKind::GitHub | ForgeKind::GitLab => Ok(false),
            ForgeKind::Forgejo => Ok(true),
            ForgeKind::Bitbucket => bail!("Bitbucket has no API for attaching files"),
        }
    }

    /// The body to post `body` with, its files uploaded to the repository;
    /// where the forge keeps files per item or comment, a body naming them,
    /// and the files to attach once that exists.
    pub(crate) async fn new_body(
        &self,
        account: &Account,
        body: &str,
        attachments: &[Attachment],
    ) -> Result<(String, Option<Pending>)> {
        if attachments.is_empty() {
            return Ok((body.to_owned(), None));
        }
        check_all(attachments)?;
        if !self.uploads_per_item()? {
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

    /// Upload `attachments` to `holder` and embed them in `body`.
    pub(crate) async fn attach(
        &self,
        account: &Account,
        holder: Holder,
        body: &str,
        attachments: &[Attachment],
    ) -> Result<String> {
        check_all(attachments)?;
        let repository = match self.kind {
            ForgeKind::GitHub => Some(self.repository_id(account).await?),
            _ => None,
        };
        let mut urls = Vec::new();
        for attachment in attachments {
            let request = self.upload_request(holder, attachment, repository)?;
            let response = self
                .send(account, &request)
                .await
                .with_context(|| format!("could not upload {}", attachment.path.display()))?;
            urls.push(self.uploaded_url(&response)?);
        }
        Ok(embed(body, attachments, |index, alt| {
            self.markup(&attachments[index], alt, &urls[index])
        }))
    }

    /// GitHub uploads name the repository by its numeric ID.
    async fn repository_id(&self, account: &Account) -> Result<u64> {
        let repository = self
            .send(account, &Request::get(format!("repos/{}", self.path)))
            .await?;
        repository["id"]
            .as_u64()
            .context("GitHub did not report the repository's ID")
    }

    fn upload_request(
        &self,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request> {
        let content_type = attachment.content_type()?;
        let name = encode(attachment.name(), false);
        let upload = |field| Upload {
            path: attachment.path.clone(),
            content_type,
            field,
        };
        Ok(match (self.kind, holder) {
            (ForgeKind::GitHub, Holder::Repository) => {
                let repository = repository.context("GitHub uploads need the repository's ID")?;
                let content = encode(content_type, false);
                Request::upload(
                    format!(
                        "https://uploads.{}/user-attachments/assets?name={name}&content_type={content}&repository_id={repository}",
                        self.host
                    ),
                    upload(None),
                )
            }
            (ForgeKind::GitLab, Holder::Repository) => {
                Request::upload(self.repo_endpoint("uploads"), upload(Some("file")))
            }
            (ForgeKind::Forgejo, Holder::Item(number)) => Request::upload(
                self.repo_endpoint(&format!("issues/{number}/assets?name={name}")),
                upload(Some("attachment")),
            ),
            (ForgeKind::Forgejo, Holder::Comment(id)) => Request::upload(
                self.repo_endpoint(&format!("issues/comments/{id}/assets?name={name}")),
                upload(Some("attachment")),
            ),
            _ => bail!("this forge cannot attach files to {holder:?}"),
        })
    }

    /// Where the upload's response says the file is served.
    fn uploaded_url(&self, response: &Value) -> Result<String> {
        let field = match self.kind {
            ForgeKind::Forgejo => "browser_download_url",
            _ => "url",
        };
        response[field]
            .as_str()
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .context("the forge did not report where it saved the file")
    }

    /// The Markdown that shows the uploaded file: an image, or a video player
    /// as each forge renders one.
    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match (attachment.media(), self.kind) {
            (Media::Video, ForgeKind::GitHub) => url.to_owned(),
            (Media::Video, ForgeKind::Forgejo) => format!("<video src=\"{url}\" controls></video>"),
            _ => format!("![{}]({url})", alt.replace(['[', ']'], "")),
        }
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

/// The inline images of `body`, each on one line.
fn images(body: &str) -> Vec<Image<'_>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = body[from..].find("![").map(|offset| from + offset) {
        from = start + 2;
        if let Some(image) = image_at(body, start) {
            from = image.range.end;
            found.push(image);
        }
    }
    found
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
mod tests {
    use super::*;

    fn attachment(path: &str, alt: Option<&str>) -> Attachment {
        Attachment {
            path: PathBuf::from(path),
            alt: alt.map(str::to_owned),
        }
    }

    #[test]
    fn arguments_split_the_alt_text_at_the_first_hash() {
        let cwd = Path::new("/work");
        assert_eq!(
            Attachment::parse("shots/a.png#The login error", cwd),
            attachment("/work/shots/a.png", Some("The login error"))
        );
        assert_eq!(
            Attachment::parse("/tmp/a.png#", cwd),
            attachment("/tmp/a.png", None)
        );
    }

    #[test]
    fn checks_refuse_other_types_empty_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, len: u64| {
            let path = dir.path().join(name);
            std::fs::File::create(&path).unwrap().set_len(len).unwrap();
            Attachment { path, alt: None }
        };
        let error = |attachment: Attachment| format!("{:#}", attachment.check().unwrap_err());
        assert!(write("a.PNG", 10).check().is_ok());
        assert!(write("b.mp4", MAX_IMAGE_BYTES + 1).check().is_ok());
        assert_eq!(write("b.mp4", 1).media(), Media::Video);
        assert!(error(write("c.txt", 10)).contains("image or video"));
        assert!(error(write("d.png", 0)).contains("is empty"));
        assert!(error(write("e.png", MAX_IMAGE_BYTES + 1)).contains("at most 10 MiB"));
        assert!(error(write("f.webm", MAX_VIDEO_BYTES + 1)).contains("at most 100 MiB"));
        assert!(error(attachment("relative.png", None)).contains("absolute"));
        let twice = [write("g.png", 1), write("g.png", 1)];
        assert!(format!("{:#}", check_all(&twice).unwrap_err()).contains("twice"));
    }

    #[test]
    fn references_resolve_against_the_first_directory_that_has_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes");
        std::fs::create_dir(&notes).unwrap();
        std::fs::write(notes.join("a.png"), "x").unwrap();
        std::fs::write(dir.path().join("b.mp4"), "x").unwrap();
        let attachments = [
            Attachment::parse("notes/a.png", dir.path()),
            Attachment::parse("b.mp4", dir.path()),
        ];
        let body = "See ![login](./a.png) and ![](b.mp4 \"demo\"), not ![c](c.png).";
        let resolved = resolve_references(body, &attachments, &[&notes, dir.path()]);
        assert_eq!(
            resolved,
            format!(
                "See ![login](<{}>) and ![](<{}>), not ![c](c.png).",
                attachments[0].path.display(),
                attachments[1].path.display()
            )
        );
    }

    #[test]
    fn embedding_replaces_references_and_appends_the_rest() {
        let attachments = [
            attachment("/t/a.png", Some("given")),
            attachment("/t/b.png", None),
            attachment("/t/c.mp4", None),
        ];
        let body = "Before ![shown](</t/a.png>) and ![](/t/b.png).\n";
        let embedded = embed(body, &attachments, |index, alt| {
            format!("[{alt}|{}]", attachments[index].name())
        });
        assert_eq!(
            embedded,
            "Before [shown|a.png] and [b.png|b.png].\n\n[c.mp4|c.mp4]"
        );
        let only = embed("", &attachments[..1], |_, alt| alt.to_owned());
        assert_eq!(only, "given");
    }

    #[test]
    fn forges_keep_uploads_per_repository_item_or_comment() {
        let png = attachment("/t/a b.png", None);
        let video = attachment("/t/c.mp4", None);
        let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
        assert!(!gitlab.uploads_per_item().unwrap());
        let request = gitlab
            .upload_request(Holder::Repository, &png, None)
            .unwrap();
        assert_eq!(request.endpoint, "projects/group%2Frepo/uploads");
        assert_eq!(request.upload.unwrap().field, Some("file"));
        assert_eq!(
            gitlab.markup(&video, "c", "/uploads/x/c.mp4"),
            "![c](/uploads/x/c.mp4)"
        );

        let github = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        assert!(
            github
                .upload_request(Holder::Repository, &png, None)
                .is_err()
        );
        let request = github
            .upload_request(Holder::Repository, &png, Some(42))
            .unwrap();
        assert_eq!(
            request.endpoint,
            "https://uploads.github.com/user-attachments/assets?name=a%20b.png&content_type=image%2Fpng&repository_id=42"
        );
        assert_eq!(request.upload.unwrap().field, None);
        assert_eq!(github.markup(&png, "a [b]", "u"), "![a b](u)");

        let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        assert!(forgejo.uploads_per_item().unwrap());
        assert!(
            forgejo
                .upload_request(Holder::Repository, &png, None)
                .is_err()
        );

        let bitbucket = ForgeRepo::parse("git@bitbucket.org:team/repo.git").unwrap();
        let error = bitbucket.uploads_per_item().unwrap_err();
        assert!(format!("{error:#}").contains("no API for attaching files"));
    }

    #[test]
    fn images_need_a_target_and_a_closing_parenthesis_on_their_line() {
        let targets = |body| -> Vec<String> {
            images(body)
                .iter()
                .map(|image| image.target.to_owned())
                .collect()
        };
        assert_eq!(
            targets("![a](x.png) ![b](<y z.png> 'title')"),
            ["x.png", "y z.png"]
        );
        assert!(targets("![a](x.png\n)").is_empty());
        assert!(targets("![a] (x.png)").is_empty());
        assert!(targets("![a](x.png trailing)").is_empty());
        assert_eq!(targets("![[nested]](x.png) ![b](y.png)"), ["y.png"]);
    }
}
