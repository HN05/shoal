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

/// The full name of the ref the symbolic ref `reference` leads to, as `git
/// symbolic-ref` gives it: the end of its chain of symbolic refs, whether
/// or not that ref exists; `None` when `reference` is missing or not
/// symbolic.
pub(crate) async fn symbolic_target(repo: &Path, reference: &str) -> Result<Option<String>> {
    let owned = reference.to_owned();
    let in_process = move |repo: &gix::Repository| Ok(followed(repo, &owned)?.target);
    let git = async |repo: &Path| {
        let mut command = super::command(repo);
        command.args(["symbolic-ref", "--quiet", reference]);
        // Git exits 1 for a ref that is missing or not symbolic.
        let target = super::output_unless(command, 1).await?;
        Ok(target.map(|target| target.trim_end().to_owned()))
    };
    reader::read(repo, in_process, git).await
}

/// The peels gix applies as Git does, with the kind each peels to.
const PEELS: [(&str, gix::object::Kind); 2] = [
    ("^{commit}", gix::object::Kind::Commit),
    ("^{tree}", gix::object::Kind::Tree),
];

/// `rev` as gix reads it as Git does: `HEAD`, a full ref name or a full
/// object ID, which it names, then at most one `^{commit}` or `^{tree}`
/// that peels it; `None` for any other revision syntax.
fn simple(rev: &str) -> Option<(&str, Option<gix::object::Kind>)> {
    let peeled = PEELS
        .iter()
        .find_map(|(suffix, kind)| Some((rev.strip_suffix(suffix)?, Some(*kind))));
    let (name, peel) = peeled.unwrap_or((rev, None));
    let is_object_id =
        matches!(name.len(), 40 | 64) && name.bytes().all(|byte| byte.is_ascii_hexdigit());
    let is_ref = name.starts_with("refs/") && gix::validate::reference::name(name.into()).is_ok();
    (name == "HEAD" || is_object_id || is_ref).then_some((name, peel))
}

/// The most symbolic refs Git follows from one ref.
const MAX_SYMBOLIC_DEPTH: usize = 5;

/// Where following symbolic refs from a ref ends, as Git follows them.
pub(crate) struct Followed {
    /// The full name of the last ref the chain reaches; `None` when the ref
    /// it starts from is missing or names an object itself.
    pub(crate) target: Option<String>,
    /// The object that ref names; `None` when it is missing.
    pub(crate) object: Option<gix::ObjectId>,
}

/// [`Followed`] from the ref `name`, which may be `HEAD`.
pub(crate) fn followed(repo: &gix::Repository, name: &str) -> Result<Followed> {
    let mut current = name.to_owned();
    for _ in 0..=MAX_SYMBOLIC_DEPTH {
        let target = (current != name).then(|| current.clone());
        let found = repo.try_find_reference(current.as_str())?;
        // A partial name may find a ref of another name; only the exact one
        // counts.
        let Some(found) = found.filter(|found| found.name().as_bstr() == current) else {
            return Ok(Followed {
                target,
                object: None,
            });
        };
        match found.target() {
            gix::refs::TargetRef::Symbolic(next) => current = next.as_bstr().to_string(),
            gix::refs::TargetRef::Object(id) => {
                let object = Some(id.to_owned());
                return Ok(Followed { target, object });
            }
        }
    }
    anyhow::bail!("{name} leads through more than {MAX_SYMBOLIC_DEPTH} symbolic refs")
}

/// `in_process`'s answer for the revision `rev` in the repository Git finds
/// from `path`, where gix reads `rev` as Git does: `HEAD`, a full ref name
/// or an object ID of the repository's full length, peeled at most once to
/// a commit or tree. `git`'s answer for any other revision, such as one
/// with syntax Git must interpret or an abbreviated object ID.
pub(crate) async fn read_revision<T: Send + 'static>(
    path: &Path,
    rev: &str,
    in_process: impl FnOnce(&gix::Repository, &str) -> Result<T> + Send + 'static,
    git: impl AsyncFnOnce(&Path) -> Result<T>,
) -> Result<T> {
    if simple(rev).is_none() {
        return git(path).await;
    }
    let owned = rev.to_owned();
    let in_process = move |repo: &gix::Repository| {
        let alike = reads_alike(repo, &owned);
        alike.then(|| in_process(repo, &owned)).transpose()
    };
    reader::read_unless_deferred(path, in_process, git).await
}

/// Whether gix reads the simple revision `rev` in `repo` as Git does: an
/// object ID only at the length of the repository's object format, since a
/// SHA-1 length abbreviates a SHA-256 ID.
fn reads_alike(repo: &gix::Repository, rev: &str) -> bool {
    simple(rev).is_some_and(|(name, _)| {
        name == "HEAD" || name.starts_with("refs/") || name.len() == repo.object_hash().len_in_hex()
    })
}

/// The object `rev`, which [`read_revision`] reads in process, names in `repo`, as
/// `git rev-parse` gives it: unpeeled unless it asks for a peel, and a full
/// object ID as it is, whether or not the repository has it. `None` when
/// it names no ref or no object of the kind it peels to, or HEAD names no
/// commit yet.
pub(crate) fn named_object(repo: &gix::Repository, rev: &str) -> Result<Option<gix::ObjectId>> {
    let (name, peel) = simple(rev).with_context(|| format!("{rev} is no ref or object"))?;
    let Some(object) = unpeeled(repo, name)? else {
        return Ok(None);
    };
    let Some(kind) = peel else {
        return Ok(Some(object));
    };
    let Some(object) = repo.try_find_object(object)? else {
        return Ok(None);
    };
    Ok(object.peel_to_kind(kind).ok().map(|object| object.id))
}

fn unpeeled(repo: &gix::Repository, name: &str) -> Result<Option<gix::ObjectId>> {
    if name == "HEAD" {
        let commit = super::queries::head_in_process(repo)?.commit;
        return Ok(commit
            .map(|commit| gix::ObjectId::from_hex(commit.as_str().as_bytes()))
            .transpose()?);
    }
    if !name.starts_with("refs/") {
        return Ok(Some(gix::ObjectId::from_hex(name.as_bytes())?));
    }
    match repo.try_find_reference(name)? {
        Some(mut found) if found.name().as_bstr() == name => {
            Ok(Some(found.follow_to_object()?.detach()))
        }
        _ => Ok(None),
    }
}

/// The commit `rev`, which [`read_revision`] reads in process, peels to in `repo`;
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

/// The commit `rev`, which [`read_revision`] reads in process, peels to and the full
/// name of the ref it names, as `git rev-parse --symbolic-full-name` gives
/// it: `HEAD` for a detached HEAD, empty for an object ID; `None` when `rev`
/// peels to no commit.
pub(crate) fn named_commit(
    repo: &gix::Repository,
    rev: &str,
) -> Result<Option<(CommitId, String)>> {
    let Some(commit) = peeled_commit(repo, rev)? else {
        return Ok(None);
    };
    let name = match simple(rev) {
        Some(("HEAD", None)) => super::queries::head_in_process(repo)?
            .reference
            .unwrap_or_else(|| "HEAD".to_owned()),
        Some((name, None)) if name.starts_with("refs/") => followed(repo, name)?
            .target
            .unwrap_or_else(|| name.to_owned()),
        _ => String::new(),
    };
    Ok(Some((commit, name)))
}

#[cfg(test)]
mod tests;
