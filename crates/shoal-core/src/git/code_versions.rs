//! The versions of files that code references keep: blobs written into the
//! repository and kept reachable under one ref per space, since a ref per
//! blob would slow every Git command that lists refs, and where a file went
//! when it was renamed whole.
use std::path::Path;

use anyhow::Result;
use shoal_refs::CommitId;

use super::{run_isolated, temporary_index::TemporaryIndex};

const REFS: &str = "refs/shoal/threads/";

/// The ref that keeps the versions the records of the space `space` keep.
pub fn reference(space: &str) -> String {
    format!("{REFS}{space}")
}

/// The spaces whose refs keep versions in the repository.
pub async fn spaces(repo: &Path) -> Result<Vec<String>> {
    let refs = super::refs::list(repo, &[REFS]).await?;
    Ok((refs.into_iter())
        .filter_map(|reference| Some(reference.name.strip_prefix(REFS)?.to_owned()))
        .collect())
}

/// The blobs `reference` keeps, sorted; none without the ref.
pub async fn kept(repo: &Path, reference: &str) -> Result<Vec<String>> {
    if !super::ref_exists(repo, reference).await? {
        return Ok(Vec::new());
    }
    let tree = run_isolated(repo, &["ls-tree", "--object-only", reference]).await?;
    let mut blobs: Vec<String> = tree.lines().map(str::to_owned).collect();
    blobs.sort();
    Ok(blobs)
}

/// Write `contents` into the repository as a blob, as they are, without
/// Git's filters; its object ID.
pub async fn write(repo: &Path, contents: impl Into<Vec<u8>>) -> Result<String> {
    let mut command = super::isolated_command(repo);
    command.args(["hash-object", "-w", "--no-filters", "--stdin"]);
    let blob = super::bounded(command).input(contents).output().await?;
    Ok(blob.trim().to_owned())
}

pub async fn read(repo: &Path, blob: &str) -> Result<String> {
    run_isolated(repo, &["cat-file", "blob", blob]).await
}

/// The blob each of the files `paths` of the worktree would be, in order,
/// as [`write`] writes them.
pub async fn hash(worktree: &Path, paths: &[&str]) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let args = [&["hash-object", "--no-filters", "--"][..], paths].concat();
    let blobs = run_isolated(worktree, &args).await?;
    Ok(blobs.lines().map(str::to_owned).collect())
}

/// Keep `blobs` reachable under `reference` with a commit whose tree holds
/// each, replacing what it kept; without any, delete the ref.
pub async fn keep(repo: &Path, reference: &str, blobs: &[String]) -> Result<()> {
    if blobs.is_empty() {
        run_isolated(repo, &["update-ref", "-d", reference]).await?;
        return Ok(());
    }
    let entries: String = (blobs.iter())
        .map(|blob| format!("100644 blob {blob}\t{blob}\n"))
        .collect();
    let mut command = super::isolated_command(repo);
    command.arg("mktree");
    let tree = super::bounded(command).input(entries).output().await?;
    let index = TemporaryIndex::empty(repo)?;
    let message = "Versions of the code Shoal's threads point at";
    let commit = index
        .run(&["commit-tree", tree.trim(), "-m", message])
        .await?;
    run_isolated(repo, &["update-ref", reference, commit.trim()]).await?;
    Ok(())
}

/// Where the file `path` went if it was renamed whole since `commit`, by
/// Git's rename detection between `commit` with `blob` as that file and
/// the worktree's files now.
pub async fn renamed(
    worktree: &Path,
    commit: &CommitId,
    path: &str,
    blob: &str,
) -> Result<Option<String>> {
    let before = TemporaryIndex::empty(worktree)?;
    before.run(&["read-tree", commit.as_str()]).await?;
    let entry = format!("100644,{blob},{path}");
    before
        .run(&["update-index", "--add", "--cacheinfo", &entry])
        .await?;
    let before = before.run(&["write-tree"]).await?;
    let now = TemporaryIndex::copy_of(worktree).await?.tree().await?;
    let renames = run_isolated(
        worktree,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--name-status",
            "--find-renames",
            "--diff-filter=R",
            before.trim(),
            &now,
            "--",
        ],
    )
    .await?;
    // Each rename is its status, its old path and its new path.
    let fields: Vec<&str> = renames.split('\0').collect();
    Ok((fields.as_chunks::<3>().0.iter())
        .find(|[_, from, _]| *from == path)
        .map(|[_, _, to]| (*to).to_owned()))
}

#[cfg(test)]
mod tests;
