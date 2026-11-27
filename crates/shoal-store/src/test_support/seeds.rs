//! The rows of the shared tables a test records, each holding what its
//! record gives and else the least the schema accepts, so that no test
//! writes the columns of another subsystem's table itself.
use std::path::Path;

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use shoal_ids::space_ids::{SpaceId, SpaceNumber};
use shoal_protocol::{
    model::{Chat, Checkout, Execution, Head, Repository, Space, SpaceKind, Workspace},
    scope_tokens::ScopeToken,
    state::ExecutionState,
    test_support,
};

use crate::{json_text, narrowing_column};

/// Record `repository`, unless a repository of its ID is recorded.
pub fn repository(db: &Connection, repository: &Repository) -> Result<()> {
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used,name,workspaces_dir,
            contribution,shared_url) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
            ON CONFLICT(id) DO NOTHING",
        params![
            repository.id,
            repository.path.to_str(),
            repository.source,
            repository.last_used,
            repository.name,
            repository.workspaces_dir.to_str(),
            repository.contribution,
            repository.shared_url,
        ],
    )?;
    Ok(())
}

/// Record `workspace`, whose worktree Git created and whose setup has yet
/// to finish, and return the number the store gives it in place of the
/// record's.
pub fn workspace(db: &Connection, workspace: &Workspace) -> Result<SpaceNumber> {
    let Checkout::Created { base, git_dir } = &workspace.checkout else {
        bail!("a seeded workspace has its worktree");
    };
    let (branch, start_commit, made_for) = match &workspace.head {
        Head::Branch(branch) => (Some(branch), None, None),
        Head::Commit(start) => (None, Some(&start.commit), start.made_for.as_ref()),
    };
    in_savepoint(db, || {
        db.execute(
            "INSERT INTO workspaces(id,path,branch,observed_branch,start_commit,made_for,state,
            error,base_commit,base_ref,git_dir,setup_skipped,disposable)
            VALUES (?1,?2,?3,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                workspace.id,
                workspace.path.to_str(),
                branch,
                start_commit,
                json_text(made_for)?,
                workspace.state,
                workspace.error,
                base.commit,
                base.reference,
                git_dir.to_str(),
                workspace.setup_skipped,
                workspace.disposable,
            ],
        )?;
        space(db, workspace, SpaceKind::Workspace)
    })
}

/// Record a ready workspace `id` on the branch `branch` at `/work/<id>`, of
/// the repository `repository` at `/<repository>`, which it records unless
/// it is recorded.
pub fn ready_workspace(
    db: &Connection,
    repository: &str,
    id: &str,
    branch: &str,
) -> Result<Workspace> {
    let checkout = Path::new("/").join(repository);
    self::repository(db, &test_support::repository(repository, &checkout))?;
    let path = Path::new("/work").join(id);
    let mut recorded = test_support::workspace(id, repository, branch, &path);
    recorded.number = workspace(db, &recorded)?;
    Ok(recorded)
}

/// Record `chat` and return the number the store gives it in place of the
/// record's.
pub fn chat(db: &Connection, chat: &Chat) -> Result<SpaceNumber> {
    in_savepoint(db, || {
        db.execute("INSERT INTO chats(id) VALUES (?1)", [&chat.id])?;
        space(db, chat, SpaceKind::Chat)
    })
}

/// Run `record` in a savepoint of its own, which on a connection outside a
/// transaction is one: a space's kind row goes in before its `spaces` row,
/// which journals both, so the key between them holds only at its end.
fn in_savepoint<T>(db: &Connection, record: impl FnOnce() -> Result<T>) -> Result<T> {
    db.execute_batch("SAVEPOINT seed")?;
    let recorded = record();
    match &recorded {
        Ok(_) => db.execute_batch("RELEASE seed")?,
        Err(_) => db.execute_batch("ROLLBACK TO seed; RELEASE seed")?,
    }
    recorded
}

/// The row in `spaces` that the row of its kind extends.
fn space<P>(db: &Connection, space: &Space<P>, kind: SpaceKind) -> Result<SpaceNumber> {
    db.execute(
        "INSERT INTO spaces(id,kind,repository_id,name,mode,narrowing) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            space.id,
            kind,
            space.repository_id,
            space.name,
            space.mode,
            narrowing_column(&space.narrowing)?,
        ],
    )?;
    Ok(db.query_row(
        "SELECT number FROM spaces WHERE id=?1",
        [&space.id],
        |row| row.get(0),
    )?)
}

/// Record `execution`, with the scope token its agent acts with when it
/// gives one.
pub fn execution(
    db: &Connection,
    execution: &Execution,
    scope_token: Option<&ScopeToken>,
) -> Result<()> {
    db.execute(
        "INSERT INTO executions(id,space_id,state,wrapper,child,group_id,origin,agent,model,
            effort,mode,narrowing,scope_token) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            execution.id,
            execution.space_id,
            execution.state,
            json_text(execution.wrapper.as_ref())?,
            json_text(execution.child.as_ref())?,
            execution.group_id,
            json_text(execution.origin.as_ref())?,
            execution.agent,
            execution.model.model,
            execution.model.effort,
            execution.mode,
            narrowing_column(&execution.narrowing)?,
            scope_token,
        ],
    )?;
    Ok(())
}

/// Record the execution `id` of the space `space` in `state`, which
/// registered nothing of its processes or agent.
pub fn bare_execution(
    db: &Connection,
    id: &str,
    space: &SpaceId,
    state: ExecutionState,
) -> Result<()> {
    let mut recorded = test_support::execution(id, space.as_str());
    recorded.state = state;
    execution(db, &recorded, None)
}

/// Record the space `child` below `parent`.
pub fn parent(db: &Connection, child: &SpaceId, parent: &SpaceId) -> Result<()> {
    db.execute(
        "INSERT INTO space_parents(space_id,parent_id) VALUES (?1,?2)",
        [child, parent],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
