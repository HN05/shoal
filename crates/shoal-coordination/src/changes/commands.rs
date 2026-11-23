//! A command's changes: the worktree's snapshot when the agent's command
//! starts is the base its changes are diffed against when it ends, and
//! what changed since the agent's last tool call is someone else's. While
//! one of its commands runs, every change is the agent's.
use crate::Coordination;
use std::collections::BTreeSet;

use anyhow::Result;
use shoal_protocol::changes::{Change, ChangeAuthor};

use super::{new_changes, records};
use crate::{git, model::Workspace, time::UnixTime};

impl Coordination {
    /// The command `call` of `workspace`'s agent starts: record what
    /// changed since its last tool call as someone else's, and diff the
    /// command's changes against the worktree now.
    pub(super) async fn command_starts(
        &self,
        workspace: &Workspace,
        call: String,
    ) -> Result<Vec<Change>> {
        let id = workspace.id.clone();
        let running = (self.store)
            .read(move |tx| records::commands(tx, &id))
            .await?;
        let id = workspace.id.clone();
        let at = UnixTime::now();
        if !running.is_empty() {
            // Another command's base already covers this one.
            (self.store)
                .write(move |tx| records::start_command(tx, &id, &call, at))
                .await?;
            return Ok(Vec::new());
        }
        let tree = git::changes::snapshot(&workspace.path).await?;
        let others = match self.change_base(workspace).await? {
            Some(base) => git::changes::changed(&workspace.path, &base, &tree).await?,
            None => Vec::new(),
        };
        let others = new_changes(others, |_| ChangeAuthor::Others);
        let (made, outside) = (self.made_in(workspace).await?, self.records.clone());
        (self.store)
            .write(move |tx| {
                let recorded = records::insert(tx, &*outside, &made, &others)?;
                records::set_base(tx, &id, &tree)?;
                records::start_command(tx, &id, &call, at)?;
                Ok(recorded)
            })
            .await
    }

    /// The command `call` of `workspace`'s agent ended: what changed since
    /// the base is its change, mixed in a file an editor reported the user
    /// edited while it ran.
    pub(super) async fn command_ends(
        &self,
        workspace: &Workspace,
        call: String,
    ) -> Result<Vec<Change>> {
        let id = workspace.id.clone();
        let commands = (self.store)
            .read(move |tx| records::commands(tx, &id))
            .await?;
        let base = self.change_base(workspace).await?;
        let tree = git::changes::snapshot(&workspace.path).await?;
        let changed = match &base {
            Some(base) => git::changes::changed(&workspace.path, base, &tree).await?,
            None => Vec::new(),
        };
        let edited = self.edited_during(workspace, &commands, &call).await?;
        let changes = new_changes(changed, |path| match edited.contains(path) {
            true => ChangeAuthor::Mixed,
            false => ChangeAuthor::Agent,
        });
        let (made, outside) = (self.made_in(workspace).await?, self.records.clone());
        let id = workspace.id.clone();
        (self.store)
            .write(move |tx| {
                let recorded = records::insert(tx, &*outside, &made, &changes)?;
                records::set_base(tx, &id, &tree)?;
                records::end_command(tx, &id, &call)?;
                Ok(recorded)
            })
            .await
    }

    /// Who made what changed in `workspace` since its agent's last tool
    /// call: the agent while one of its commands runs, else someone else.
    pub(super) async fn author_between_calls(&self, workspace: &Workspace) -> Result<ChangeAuthor> {
        let id = workspace.id.clone();
        let running = (self.store)
            .read(move |tx| records::commands(tx, &id))
            .await?;
        Ok(match running.is_empty() {
            true => ChangeAuthor::Others,
            false => ChangeAuthor::Agent,
        })
    }

    /// The paths of `workspace` an editor reported the user edited while
    /// its agent's command `call`, among those that run, `commands`, ran.
    async fn edited_during(
        &self,
        workspace: &Workspace,
        commands: &[(String, UnixTime)],
        call: &str,
    ) -> Result<BTreeSet<String>> {
        let started = (commands.iter())
            .find(|(running, _)| running == call)
            .map_or_else(UnixTime::now, |(_, at)| *at);
        let id = workspace.id.clone();
        let edited = (self.store).read(move |tx| records::edited_since(tx, &id, started));
        Ok(edited.await?.into_iter().collect())
    }
}
