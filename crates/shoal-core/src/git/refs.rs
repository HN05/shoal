//! Ref lookups and listings, read in this process where the
//! [`reader`](super::reader) reads the repository, else by Git.
use std::path::Path;

use anyhow::{Context, Result};
use shoal_refs::CommitId;

use super::reader;

/// A ref a listing names, as `git for-each-ref` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    /// Its full name, such as `refs/heads/main`.
    pub(crate) name: String,
    /// The object it names, through a symbolic ref's target.
    pub(crate) object: String,
    /// The full name of the ref a symbolic ref points to.
    pub(crate) symref: Option<String>,
}

/// The refs `patterns` match, sorted by name: each pattern a full ref name
/// that matches that ref and the refs below it, by whole components, as
/// `git for-each-ref` matches it. A ref that names no object, such as a
/// symbolic ref to a missing one, is left out, as Git leaves it out with a
/// warning.
pub(crate) async fn list(repo: &Path, patterns: &[&str]) -> Result<Vec<Listed>> {
    // Without patterns, for-each-ref would list every ref.
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let owned: Vec<String> = patterns
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .collect();
    let in_process = move |repo: &gix::Repository| list_in_process(repo, &owned);
    reader::read(repo, in_process, async |repo| {
        list_from_git(repo, patterns).await
    })
    .await
}

fn list_in_process(repo: &gix::Repository, patterns: &[String]) -> Result<Vec<Listed>> {
    let platform = repo.references()?;
    let mut listed = Vec::new();
    for pattern in patterns {
        for reference in platform.prefixed(pattern.as_str())?.flatten() {
            let name = reference.name().as_bstr().to_string();
            if matches(pattern, &name)
                && let Some(reference) = listed_in_process(reference, name)
            {
                listed.push(reference);
            }
        }
    }
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    listed.dedup_by(|a, b| a.name == b.name);
    Ok(listed)
}

/// `reference`, named `name`, as a listing shows it; `None` when it names
/// no object.
fn listed_in_process(mut reference: gix::Reference<'_>, name: String) -> Option<Listed> {
    let symref = match reference.target() {
        gix::refs::TargetRef::Symbolic(target) => Some(target.as_bstr().to_string()),
        gix::refs::TargetRef::Object(_) => None,
    };
    let object = reference.follow_to_object().ok()?.to_string();
    Some(Listed {
        name,
        object,
        symref,
    })
}

/// Whether the for-each-ref `pattern` matches the ref `name`: the name
/// itself, or one below it.
fn matches(pattern: &str, name: &str) -> bool {
    name.strip_prefix(pattern)
        .is_some_and(|rest| rest.is_empty() || pattern.ends_with('/') || rest.starts_with('/'))
}

/// The [`list`] Git prints for `repo`.
async fn list_from_git(repo: &Path, patterns: &[&str]) -> Result<Vec<Listed>> {
    let mut args = vec![
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(symref)",
    ];
    args.extend(patterns);
    let listing = super::run(repo, &args).await?;
    listing
        .lines()
        .map(|line| {
            let mut fields = line.split('\0');
            let mut field = || {
                fields
                    .next()
                    .context("git for-each-ref printed a short line")
            };
            let (name, object, symref) = (field()?, field()?, field()?);
            Ok(Listed {
                name: name.to_owned(),
                object: object.to_owned(),
                symref: (!symref.is_empty()).then(|| symref.to_owned()),
            })
        })
        .collect()
}

/// Whether the exact ref `reference`, a full name, exists in `repo`; a
/// name no ref can have, such as a revision expression, names none.
pub(crate) fn exists_in_process(repo: &gix::Repository, reference: &str) -> Result<bool> {
    if gix::validate::reference::name(reference.into()).is_err() {
        return Ok(false);
    }
    let found = repo.try_find_reference(reference)?;
    // A partial name may match a ref of another name, such as one below
    // `refs/heads/`; only the exact name counts.
    Ok(found.is_some_and(|found| found.name().as_bstr() == reference))
}

/// The full name of the ref the symbolic ref `reference` points to, as
/// `git symbolic-ref` gives it, whether or not that ref exists; `None` when
/// `reference` is missing or not symbolic.
pub(crate) async fn symbolic_target(repo: &Path, reference: &str) -> Result<Option<String>> {
    let owned = reference.to_owned();
    let in_process = move |repo: &gix::Repository| {
        let found = repo.try_find_reference(owned.as_str())?;
        Ok(found.and_then(|found| match found.target() {
            gix::refs::TargetRef::Symbolic(target) if found.name().as_bstr() == owned => {
                Some(target.as_bstr().to_string())
            }
            _ => None,
        }))
    };
    let git = async |repo: &Path| {
        let mut command = super::command(repo);
        command.args(["symbolic-ref", "--quiet", reference]);
        // Git exits 1 for a ref that is missing or not symbolic.
        let target = super::output_unless(command, 1).await?;
        Ok(target.map(|target| target.trim_end().to_owned()))
    };
    reader::read(repo, in_process, git).await
}

/// Whether gix reads `rev` as Git does: `HEAD`, a full ref name or a full
/// object ID, with no revision syntax to interpret.
pub(crate) fn names_ref_or_object(rev: &str) -> bool {
    let is_object_id =
        matches!(rev.len(), 40 | 64) && rev.bytes().all(|byte| byte.is_ascii_hexdigit());
    rev == "HEAD"
        || is_object_id
        || (rev.starts_with("refs/") && gix::validate::reference::name(rev.into()).is_ok())
}

/// The object `rev`, which [`names_ref_or_object`], names in `repo`,
/// without peeling it, as `git rev-parse` gives it: a full object ID as it
/// is, whether or not the repository has it. `None` when `rev` names no ref
/// or HEAD names no commit yet.
pub(crate) fn named_object(repo: &gix::Repository, rev: &str) -> Result<Option<gix::ObjectId>> {
    if rev == "HEAD" {
        return Ok(repo.head()?.id().map(|id| id.detach()));
    }
    if !rev.starts_with("refs/") {
        return Ok(Some(gix::ObjectId::from_hex(rev.as_bytes())?));
    }
    match repo.try_find_reference(rev)? {
        Some(mut found) if found.name().as_bstr() == rev => {
            Ok(Some(found.follow_to_object()?.detach()))
        }
        _ => Ok(None),
    }
}

/// The commit `rev`, which [`names_ref_or_object`], peels to in `repo`;
/// `None` when it names nothing or no commit.
pub(crate) fn peeled_commit(repo: &gix::Repository, rev: &str) -> Result<Option<CommitId>> {
    let object = named_object(repo, rev)?.map(|id| repo.try_find_object(id));
    let Some(object) = object.transpose()?.flatten() else {
        return Ok(None);
    };
    Ok(object
        .peel_to_commit()
        .ok()
        .map(|commit| commit.id.to_string().into()))
}

#[cfg(test)]
mod tests;
