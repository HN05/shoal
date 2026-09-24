//! One removal path for explicit removal and automatic cleanup.
use super::Manager;
use crate::{
    git::worktrunk,
    hooks::{self, Hook},
    removal::{self, BranchChoice, RemovalCheck, RemovalResult},
    state::WorkspaceState,
};
use anyhow::{Result, bail, ensure};

/// Who is removing the workspace, which decides how strict the checks are.
#[derive(Clone, Copy)]
enum Removal<'a> {
    /// Explicit merge completion stops tracked agents without waiting for idle.
    Merged { head: &'a str },
    /// A user request; dirty or differing work needs an explicit branch choice.
    Manual {
        choice: BranchChoice,
        caller_pid: u32,
    },
    /// Idle cleanup; only safe, unchanged workspaces may go.
    Automatic { snapshot: u64 },
    /// The directory was deleted outside Shoal; forget the workspace and
    /// release what it owned, retaining its branch.
    Deleted,
}

impl Removal<'_> {
    fn choice(self) -> BranchChoice {
        match self {
            Removal::Manual { choice, .. } => choice,
            Removal::Automatic { .. } | Removal::Deleted | Removal::Merged { .. } => {
                BranchChoice::Auto
            }
        }
    }

    fn caller_pid(self) -> u32 {
        match self {
            Removal::Manual { caller_pid, .. } => caller_pid,
            Removal::Automatic { .. } | Removal::Deleted | Removal::Merged { .. } => 0,
        }
    }

    /// Unattended removals never signal processes Shoal cannot verify.
    fn is_automatic(self) -> bool {
        !matches!(self, Removal::Manual { .. } | Removal::Merged { .. })
    }

    /// Automatic removal needs a safe workspace; manual removal with `Auto`
    /// needs a workspace that does not require a branch choice.
    fn verify(self, check: &RemovalCheck, stage: Stage) -> Result<()> {
        match (self, stage) {
            (Removal::Merged { .. }, _) => ensure!(
                !check.dirty,
                "workspace has uncommitted or untracked work; retaining it"
            ),
            (Removal::Automatic { .. }, Stage::Initial) => ensure!(
                check.safe(),
                "workspace is no longer idle, clean and fully pushed"
            ),
            (Removal::Automatic { .. }, Stage::AfterStop) => {
                ensure!(check.safe(), "workspace changed while stopping commands")
            }
            (
                Removal::Manual {
                    choice: BranchChoice::Auto,
                    ..
                },
                Stage::Initial,
            ) => ensure!(
                !check.needs_choice(),
                "removal requires a branch choice: {}; use --yes with --keep-branch or --delete-branch",
                check.warnings().join("; ")
            ),
            (
                Removal::Manual {
                    choice: BranchChoice::Auto,
                    ..
                },
                Stage::AfterStop,
            ) => ensure!(
                !check.needs_choice(),
                "workspace changed while stopping commands; choose whether to keep or delete the branch"
            ),
            (Removal::Manual { .. } | Removal::Deleted, _) => {}
        }
        Ok(())
    }
}

/// The removal checks run twice: before and after stopping commands.
#[derive(Clone, Copy)]
enum Stage {
    Initial,
    AfterStop,
}

impl Manager {
    pub async fn check_removal(&self, selector: &str, caller_pid: u32) -> Result<RemovalCheck> {
        let inspection = self.inspect_workspace(selector).await?;
        if inspection.workspace.path.exists() {
            self.verify_worktree(&inspection.workspace).await?;
        }
        let repo = self.repository(&inspection.workspace.repository_id).await?;
        let default_branch = crate::git::default_branch::resolve(&repo.path, false)
            .await
            .ok();
        removal::inspect(
            inspection.workspace,
            inspection.executions.len(),
            caller_pid,
            default_branch.as_deref(),
        )
        .await
    }

    pub async fn remove_workspace(
        &self,
        selector: &str,
        choice: BranchChoice,
        caller_pid: u32,
    ) -> Result<RemovalResult> {
        self.remove(selector, Removal::Manual { choice, caller_pid })
            .await
    }

    /// A fingerprint of everything automatic cleanup must see unchanged before
    /// removing the workspace, or `None` when it is not a cleanup candidate.
    pub async fn cleanup_snapshot(&self, id: &str) -> Result<Option<u64>> {
        if self.pr_registration(id).await?.is_some() {
            return Ok(None);
        }
        if !self.list_resources(Some(id)).await?.is_empty() {
            return Ok(None);
        }
        if self
            .list_simulators(Some(id))
            .await?
            .iter()
            .any(|s| s.workspace_id.is_some())
        {
            return Ok(None);
        }
        let check = self.check_removal(id, 0).await?;
        if !check.safe() || !check.workspace.path.is_dir() {
            return Ok(None);
        }
        let head = crate::git::run(&check.workspace.path, &["rev-parse", "HEAD"]).await?;
        let activity = self.activity(id).await;
        let path = check.workspace.path;
        Ok(Some(
            tokio::task::spawn_blocking(move || {
                crate::daemon::cleanup::fingerprint(&path, &head, activity)
            })
            .await??,
        ))
    }

    pub async fn remove_idle(&self, selector: &str, snapshot: u64) -> Result<()> {
        self.remove(selector, Removal::Automatic { snapshot })
            .await
            .map(|_| ())
    }

    pub async fn remove_merged(&self, selector: &str, head: &str) -> Result<()> {
        self.remove(selector, Removal::Merged { head })
            .await
            .map(|_| ())
    }

    /// Forget a workspace whose directory was deleted outside Shoal.
    pub async fn remove_deleted(&self, selector: &str) -> Result<RemovalResult> {
        self.remove(selector, Removal::Deleted).await
    }

    async fn remove(&self, selector: &str, removal: Removal<'_>) -> Result<RemovalResult> {
        let workspace = self.workspace(selector).await?;
        self.reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
            .await?;
        // Checked only after the reservation, so a worktree restored meanwhile
        // cannot be deleted by an unattended removal.
        let present = workspace.path.try_exists()?;
        let result = async {
            let post_remove = if present {
                let config = self.workspace_config(&workspace).await?;
                let command = config
                    .post_remove_cmd
                    .as_ref()
                    .or(self.config.post_remove_cmd.as_ref());
                match command {
                    Some(command) => {
                        let checkout = self.repository(&workspace.repository_id).await?.path;
                        Some((checkout.join(command), checkout))
                    }
                    None => None,
                }
            } else {
                None
            };
            let outcome = match removal {
                Removal::Deleted if present => bail!("workspace directory exists"),
                _ if present => self.remove_present_worktree(&workspace, removal).await?,
                _ => self.remove_missing_worktree(&workspace, removal).await?,
            };
            self.remove_simulators(&workspace.id).await?;
            let id = workspace.id.clone();
            self.store
                .run(move |db| {
                    let tx = db.transaction()?;
                    tx.execute("DELETE FROM executions WHERE workspace_id=?1", [&id])?;
                    tx.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
                    tx.commit()?;
                    Ok((outcome, post_remove))
                })
                .await
        }
        .await;
        match result {
            Ok((mut outcome, post_remove)) => {
                self.forget_workspace(&workspace.id).await;
                // Session logs are run data owned by the record just deleted.
                let _ = std::fs::remove_dir_all(self.paths.workspace_state(&workspace.id));
                if let Some((command, checkout)) = post_remove
                    && let Err(error) = hooks::run_detached(
                        Hook::PostRemove(&checkout),
                        &workspace,
                        &command,
                        &self.paths,
                    )
                    .await
                {
                    let message = format!("workspace removed; {error:#}");
                    self.notify(
                        Some(&workspace.name),
                        crate::daemon::notifications::NotificationKind::HookFailed,
                        &message,
                    )
                    .await;
                    outcome.hook_error = Some(message);
                }
                Ok(outcome)
            }
            Err(error) => {
                // A deleted directory can no longer be ready.
                let state = match removal {
                    Removal::Deleted if !present => WorkspaceState::Failed,
                    _ => workspace.state,
                };
                self.set_state(&workspace.id, state, Some(format!("{error:#}")))
                    .await?;
                Err(error)
            }
        }
    }

    async fn remove_present_worktree(
        &self,
        workspace: &crate::model::Workspace,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let repo = self.repository(&workspace.repository_id).await?;
        let check = self
            .check_removal(&workspace.id, removal.caller_pid())
            .await?;
        removal.verify(&check, Stage::Initial)?;
        if let Removal::Merged { head } = removal {
            ensure!(
                crate::forge::pr::current_head(workspace).await? == head,
                "HEAD changed before PR cleanup"
            );
        }
        self.stop_executions(&workspace.id, !removal.is_automatic())
            .await?;
        if let Removal::Automatic { snapshot } = removal {
            ensure!(
                self.cleanup_snapshot(&workspace.id).await? == Some(snapshot),
                "workspace changed before automatic removal"
            );
        }
        let check = self
            .check_removal(&workspace.id, removal.caller_pid())
            .await?;
        removal.verify(&check, Stage::AfterStop)?;
        if let Removal::Merged { head } = removal {
            ensure!(
                crate::forge::pr::current_head(workspace).await? == head,
                "HEAD changed while stopping commands"
            );
        }
        let choice = removal.choice();
        let default_branch = crate::git::default_branch::resolve(&repo.path, false)
            .await
            .ok();
        let delete_branch = match choice {
            BranchChoice::Auto => {
                check.can_delete_branch() && check.branch.as_deref() != default_branch.as_deref()
            }
            BranchChoice::KeepBranch => false,
            BranchChoice::DeleteBranch => true,
        };
        ensure!(
            !matches!(choice, BranchChoice::KeepBranch)
                || check.branch.is_some()
                || check.unpushed_commits == 0,
            "detached HEAD has unpushed commits; create a branch before choosing to keep it"
        );
        // The hook sees the worktree intact; a failing hook retains it.
        if let Some(command) = self
            .workspace_config(workspace)
            .await?
            .hooks(&workspace.path)
            .pre_remove_cmd
        {
            hooks::run_detached(Hook::PreRemove, workspace, &command, &self.paths).await?;
        }
        let release_hook = self.resource_hook(workspace, false).await?;
        for lease in self.list_resources(Some(&workspace.id)).await? {
            self.run_resource_release_hook(workspace, &lease, release_hook.as_deref())
                .await?;
        }
        if let Removal::Merged { head } = removal {
            ensure!(
                crate::forge::pr::current_head(workspace).await? == head,
                "HEAD changed during removal hooks"
            );
            removal.verify(
                &self.check_removal(&workspace.id, 0).await?,
                Stage::AfterStop,
            )?;
        }
        // Live resources are removed before the directory; failed cleanup
        // retains their ownership records so removal can be retried.
        self.remove_simulators(&workspace.id).await?;
        worktrunk::remove(
            &repo.path,
            &self.paths.worktrunk_config(),
            &workspace.path,
            !matches!(choice, BranchChoice::Auto),
            delete_branch,
        )
        .await
    }

    /// The directory is gone: a worktree that was deleted rather than moved is
    /// forgotten, and its branch is always retained.
    async fn remove_missing_worktree(
        &self,
        workspace: &crate::model::Workspace,
        removal: Removal<'_>,
    ) -> Result<RemovalResult> {
        let repo = self.repository(&workspace.repository_id).await?;
        ensure!(
            !matches!(removal, Removal::Automatic { .. }),
            "workspace directory disappeared during idle cleanup"
        );
        ensure!(
            self.missing_worktree(workspace).await?.is_none(),
            "worktree was moved; restore its recorded path before removing it"
        );
        if matches!(removal, Removal::Deleted) {
            ensure!(
                self.inspect_workspace(&workspace.id)
                    .await?
                    .executions
                    .is_empty(),
                "commands are recorded; stop them with shoal stop or shoal rm"
            );
        }
        self.stop_executions(&workspace.id, !removal.is_automatic())
            .await?;
        self.remove_simulators(&workspace.id).await?;
        if self.is_registered_worktree(workspace).await? {
            // Prune only this owned registration, through Worktrunk, and
            // retain its branch because the contents cannot be inspected.
            worktrunk::remove(
                &repo.path,
                &self.paths.worktrunk_config(),
                &workspace.path,
                true,
                false,
            )
            .await
        } else {
            Ok(RemovalResult {
                removed: true,
                branch: Some(workspace.branch.clone()),
                branch_deleted: false,
                branch_outcome: "retained".into(),
                hook_error: None,
            })
        }
    }
}
