//! The repository and workspace row mappers, and the columns they read.
use std::path::PathBuf;

use rusqlite::Row;
use shoal_protocol::model::{Base, Checkout, Head, Repository, StartingCommit, Workspace};
use shoal_refs::CommitId;

use crate::sql::json_column;

pub const REPOSITORY_COLUMNS: &str =
    "id,path,source,last_used,name,workspaces_dir,contribution,shared_url";

pub fn repository(row: &Row<'_>) -> rusqlite::Result<Repository> {
    Ok(Repository {
        id: row.get("id")?,
        path: PathBuf::from(row.get::<_, String>("path")?),
        source: row.get("source")?,
        last_used: row.get("last_used")?,
        name: row.get("name")?,
        workspaces_dir: PathBuf::from(row.get::<_, String>("workspaces_dir")?),
        contribution: row.get("contribution")?,
        shared_url: row.get("shared_url")?,
    })
}

pub const WORKSPACE_COLUMNS: &str = "id,number,repository_id,name,mode,narrowing,path,branch,start_commit,made_for,state,error,base_commit,base_ref,git_dir,pending_plan,setup_skipped,disposable";

pub fn workspace(row: &Row<'_>) -> rusqlite::Result<Workspace> {
    let mut workspace = Workspace::record(
        row.get("id")?,
        row.get("repository_id")?,
        row.get("name")?,
        PathBuf::from(row.get::<_, String>("path")?),
        head(row)?,
        row.get("state")?,
    );
    workspace.number = row.get("number")?;
    workspace.mode = row.get("mode")?;
    workspace.narrowing = json_column(row, "narrowing")?.unwrap_or_default();
    workspace.error = row.get("error")?;
    workspace.checkout = checkout(row)?;
    workspace.setup_skipped = row.get("setup_skipped")?;
    workspace.disposable = row.get("disposable")?;
    Ok(workspace)
}

/// A workspace's branch, or the commit it started at without one, from its
/// row, which holds exactly one of them.
fn head(row: &Row<'_>) -> rusqlite::Result<Head> {
    let conversion = |error: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, error)
    };
    let made_for = match row.get::<_, Option<String>>("made_for")? {
        Some(json) => Some(serde_json::from_str(&json).map_err(|error| conversion(error.into()))?),
        None => None,
    };
    match (
        row.get("branch")?,
        row.get::<_, Option<String>>("start_commit")?,
    ) {
        (Some(branch), None) if made_for.is_none() => Ok(Head::Branch(branch)),
        (None, Some(commit)) => Ok(Head::Commit(StartingCommit {
            commit: commit.into(),
            made_for,
        })),
        _ => Err(conversion(
            "a workspace row holds exactly one of a branch and a starting commit".into(),
        )),
    }
}

/// What a workspace's row records of its worktree: pending while it holds
/// the plan of its creation, else created, which the table refuses without
/// a base and admin directory.
fn checkout(row: &Row<'_>) -> rusqlite::Result<Checkout> {
    let base = match row.get::<_, Option<CommitId>>("base_commit")? {
        Some(commit) => Some(Base {
            commit,
            reference: row.get("base_ref")?,
        }),
        None => None,
    };
    let git_dir = row.get::<_, Option<String>>("git_dir")?.map(PathBuf::from);
    match (row.get::<_, Option<String>>("pending_plan")?, base, git_dir) {
        (Some(_), base, git_dir) => Ok(Checkout::Pending { base, git_dir }),
        (None, Some(base), Some(git_dir)) => Ok(Checkout::Created { base, git_dir }),
        (None, ..) => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Null,
            "a created workspace's row holds its base and admin directory".into(),
        )),
    }
}

#[cfg(test)]
mod tests;
