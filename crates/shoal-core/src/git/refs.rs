//! Ref lookups, read in this process where the
//! [`reader`](super::reader) reads the repository, else by Git.
use std::path::Path;

use anyhow::Result;
use shoal_refs::CommitId;

use super::reader;

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
