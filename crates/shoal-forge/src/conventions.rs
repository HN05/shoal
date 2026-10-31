//! What a repository asks of its issues and PRs: labels its configuration
//! requires, a label scheme on the forge, and the forge's description
//! templates, and a PR's closing reference to the issue it works on. Shoal
//! checks them where agents open and mark items, so the repository's
//! conventions reach agents at the moment they apply.
pub mod closing;
pub mod labels;

use std::path::Path;

use anyhow::{Result, bail};

use crate::{
    ForgeKind, ForgeRepo,
    account::Account,
    backends::Backend,
    conventions::labels::Label,
    items::{item::Item, link::ItemKind},
};

/// Refuse a new item of `kind` whose labels leave a required entry unmet,
/// naming the repository's labels that would meet it.
pub async fn require(
    forge: &ForgeRepo,
    account: &Account,
    kind: ItemKind,
    required: &[String],
    labels: &[String],
) -> Result<()> {
    let missing = missing(required, labels);
    if missing.is_empty() {
        return Ok(());
    }
    // The refusal stands without the choices when they cannot be read.
    let available = forge.labels(account).await.unwrap_or_default();
    bail!(refusal(kind, &missing, &available))
}

/// What `item` leaves out of the repository's conventions: required labels,
/// or any label where the repository has some, and with its `body`, the
/// headings of the forge's templates in `worktree`. A failed lookup leaves
/// its warning out rather than failing the change that already happened.
pub async fn warnings(
    forge: &ForgeRepo,
    account: &Account,
    worktree: &Path,
    item: &Item,
    body: Option<&str>,
    required: &[String],
) -> Vec<String> {
    let missing = missing(required, &item.labels);
    let labels = if !missing.is_empty() {
        Some(missing_warning(item, &missing))
    } else if item.labels.is_empty()
        && forge
            .labels(account)
            .await
            .is_ok_and(|labels| !labels.is_empty())
    {
        Some(unlabeled_warning(item))
    } else {
        None
    };
    let template = body.and_then(|body| template_warning(forge, worktree, item, body));
    labels.into_iter().chain(template).collect()
}

/// The required entries none of `labels` satisfies; a label satisfies an
/// entry it starts with, so `area/` takes any area label.
fn missing<'a>(required: &'a [String], labels: &[String]) -> Vec<&'a str> {
    required
        .iter()
        .filter(|entry| !labels.iter().any(|label| label.starts_with(entry.as_str())))
        .map(String::as_str)
        .collect()
}

/// Why a new item of `kind` is refused, with the repository's labels that
/// would satisfy each missing entry.
fn refusal(kind: ItemKind, missing: &[&str], available: &[Label]) -> String {
    let mut message = format!(
        "a new {} needs a label starting with {} ([{kind}] required_labels); pass --label",
        kind.noun(),
        missing.join(", ")
    );
    let choices: Vec<&Label> = available
        .iter()
        .filter(|label| missing.iter().any(|entry| label.name.starts_with(entry)))
        .collect();
    if !choices.is_empty() {
        message.push_str(" with one of:");
        for label in choices {
            message.push_str(&format!("\n  {}", described(label)));
        }
    }
    message
}

/// The warning for an existing item that lacks required labels.
fn missing_warning(item: &Item, missing: &[&str]) -> String {
    format!(
        "{} has no label starting with {}; shoal repo labels lists them and {} adds one",
        name(item),
        missing.join(", "),
        edit(item, "--add-label")
    )
}

/// The warning for an item without labels in a repository that has some.
fn unlabeled_warning(item: &Item) -> String {
    format!(
        "{} has no labels; shoal repo labels lists the repository's labels and {} adds them",
        name(item),
        edit(item, "--add-label")
    )
}

/// The warning for a description that leaves out the headings of the
/// forge's template for its kind, or of every issue template; none when the
/// repository has no template or the description follows one.
fn template_warning(forge: &ForgeRepo, worktree: &Path, item: &Item, body: &str) -> Option<String> {
    let templates = templates(forge.kind, item.kind, worktree);
    let unmet = unmet(&templates, &headings(body))?;
    Some(unmet_warning(item, &unmet))
}

/// A template and the headings a description leaves out of it.
type Unmet<'a> = (&'a Template, Vec<&'a str>);

/// Each template with the headings `written` leaves out of it; none when
/// there is no template or one has all its headings written.
fn unmet<'a>(templates: &'a [Template], written: &[String]) -> Option<Vec<Unmet<'a>>> {
    let unmet: Vec<Unmet> = templates
        .iter()
        .map(|template| (template, template.missing(written)))
        .collect();
    let followed = unmet.iter().any(|(_, missing)| missing.is_empty());
    (!unmet.is_empty() && !followed).then_some(unmet)
}

/// Name the missing headings of a single template, or every template the
/// description could have followed.
fn unmet_warning(item: &Item, unmet: &[Unmet]) -> String {
    let fix = edit(item, "--body-file");
    match unmet {
        [(template, missing)] => format!(
            "the description of {} lacks the headings of {}: {}; {fix} rewrites it",
            name(item),
            template.path,
            missing.join(", ")
        ),
        several => format!(
            "the description of {} follows none of the templates {}; {fix} rewrites it",
            name(item),
            several
                .iter()
                .map(|(template, _)| template.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A description template the forge offers, with the headings it asks for.
struct Template {
    /// Relative to the worktree, with `/` separators.
    path: String,
    headings: Vec<String>,
}

impl Template {
    fn missing<'a>(&'a self, written: &[String]) -> Vec<&'a str> {
        self.headings
            .iter()
            .filter(|heading| {
                !written
                    .iter()
                    .any(|text| text.eq_ignore_ascii_case(heading))
            })
            .map(String::as_str)
            .collect()
    }
}

/// The templates the forge reads for a new item of `kind`: the first
/// directory that has one, in the forge's lookup order. Unreadable files are
/// left out; a template without headings asks for nothing.
fn templates(forge: ForgeKind, kind: ItemKind, worktree: &Path) -> Vec<Template> {
    let candidates = forge.template_lookups(kind);
    candidates
        .into_iter()
        .map(|(dir, lookup)| lookup.find(worktree, dir))
        .find(|found| !found.is_empty())
        .unwrap_or_default()
}

/// Where GitHub, Forgejo and Gitea look for templates, Forgejo's own first.
const TEMPLATE_DIRS: [&str; 5] = [".forgejo", ".gitea", ".github", "", "docs"];

/// The template lookups GitHub, Forgejo and Gitea share, in each of their
/// directories.
pub(crate) fn shared_lookups(kind: ItemKind) -> Vec<(&'static str, Lookup)> {
    match kind {
        ItemKind::Pr => TEMPLATE_DIRS
            .iter()
            .map(|dir| (*dir, Lookup::File("pull_request_template.md")))
            .collect(),
        ItemKind::Issue => TEMPLATE_DIRS
            .iter()
            .flat_map(|dir| {
                [
                    (*dir, Lookup::Directory("issue_template")),
                    (*dir, Lookup::File("issue_template.md")),
                ]
            })
            .collect(),
    }
}

/// A template file, or a directory of templates, named case-insensitively
/// as forges match them.
pub(crate) enum Lookup {
    File(&'static str),
    Directory(&'static str),
}

impl Lookup {
    fn find(&self, worktree: &Path, dir: &str) -> Vec<Template> {
        let base = worktree.join(dir);
        let relative = |file: &str| match dir {
            "" => file.to_owned(),
            dir => format!("{dir}/{file}"),
        };
        match self {
            Self::File(name) => entry_named(&base, name)
                .and_then(|file| template(&base.join(&file), relative(&file)))
                .into_iter()
                .collect(),
            Self::Directory(name) => {
                let Some(entry) = entry_named(&base, name) else {
                    return Vec::new();
                };
                markdown_files(&base.join(&entry))
                    .into_iter()
                    .filter_map(|file| {
                        let path = base.join(&entry).join(&file);
                        template(&path, relative(&format!("{entry}/{file}")))
                    })
                    .collect()
            }
        }
    }
}

/// The entry of `dir` whose name matches `name` ignoring case.
fn entry_named(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .find(|entry| entry.eq_ignore_ascii_case(name))
}

/// The Markdown files of `dir` in name order; issue forms in YAML have no
/// headings to check.
fn markdown_files(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|file| file.ok()?.file_name().into_string().ok())
        .filter(|file| file.to_ascii_lowercase().ends_with(".md"))
        .collect();
    files.sort();
    files
}

fn template(path: &Path, relative: String) -> Option<Template> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(Template {
        path: relative,
        headings: headings(without_front_matter(&text)),
    })
}

/// The text after a YAML front matter block, which issue templates use for
/// their name and default labels.
fn without_front_matter(text: &str) -> &str {
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return text;
    }
    let mut offset = text.find('\n').map_or(text.len(), |end| end + 1);
    for line in lines {
        offset += line.len();
        if line.trim_end() == "---" {
            return &text[offset..];
        }
    }
    text
}

/// The text of each Markdown ATX heading outside code fences.
fn headings(text: &str) -> Vec<String> {
    let mut fenced = false;
    let mut found = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        let level = line.bytes().take_while(|byte| *byte == b'#').count();
        let rest = &line[level..];
        if !fenced && (1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')) {
            let heading = rest.trim().trim_end_matches('#').trim();
            if !heading.is_empty() {
                found.push(heading.to_owned());
            }
        }
    }
    found
}

fn name(item: &Item) -> String {
    format!("{} #{}", item.kind.noun(), item.number)
}

fn edit(item: &Item, flag: &str) -> String {
    format!("shoal {} edit {} {flag}", item.kind, item.number)
}

fn described(label: &Label) -> String {
    match label.description.as_str() {
        "" => label.name.clone(),
        description => format!("{}: {description}", label.name),
    }
}

#[cfg(test)]
mod tests;
