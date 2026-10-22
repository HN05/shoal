//! Issues and PRs a repository's `[approvals]` makes scoped callers propose
//! instead of opening: recording them, and opening, denying or merging them
//! as duplicates once the user decides.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::TransactionBehavior::Immediate;
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        approvals::{
            ApprovalRequest, DecisionStatus, Lifetime, Specification, Target,
            proposals::{self, IssueSpecification, Recorded},
        },
        store,
        workspace::Manager,
    },
    forge::{
        ForgeRepo,
        account::Account,
        items::create::NewIssue,
        items::item::{Item, Opened},
        items::link::ItemKind,
    },
    model::Workspace,
};

/// What `issue open` or `pr open` did: opened or found the item, or
/// recorded it for the user's approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenOutcome {
    Proposed(Proposed),
    Opened(Opened),
}

/// An issue or PR waiting for the user's approval before Shoal opens it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposed {
    pub proposed: Box<ApprovalRequest>,
}

/// A scoped caller, which proposes what `[approvals]` keeps it from opening
/// directly.
#[derive(Debug, Clone, Default)]
pub struct Proposer {
    /// The agent running the command, when Shoal tracks one.
    pub agent: Option<String>,
}

impl Manager {
    /// Record `issue` for the user's approval instead of opening it.
    pub(super) async fn propose_issue(
        &self,
        workspace: &Workspace,
        issue: NewIssue,
        link: bool,
        proposer: Proposer,
    ) -> Result<Proposed> {
        let commit = crate::git::revision(&workspace.path, "HEAD", crate::git::run).await?;
        let specification = Specification::Issue(IssueSpecification {
            issue,
            link,
            branch: workspace.branch.clone(),
            commit: commit.trim().to_owned(),
            agent: proposer.agent,
        });
        let request = proposal(
            workspace,
            Target::IssueOpen,
            proposals::ISSUE,
            specification,
        );
        self.record_proposal(workspace, request, proposals::propose_issue)
            .await
    }

    /// Store the proposal with `record` and tell the user about a new one.
    async fn record_proposal(
        &self,
        workspace: &Workspace,
        request: ApprovalRequest,
        record: fn(&rusqlite::Transaction<'_>, ApprovalRequest) -> Result<Recorded>,
    ) -> Result<Proposed> {
        let (request, new) = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(Immediate)?;
                let recorded = record(&tx, request)?;
                tx.commit()?;
                Ok(recorded)
            })
            .await?;
        if new {
            self.notify_approval(&workspace.name, &request).await;
        }
        Ok(Proposed {
            proposed: Box::new(request),
        })
    }

    /// Open or deny the proposal the user chose, then forget it and tell the
    /// proposing workspace's agents.
    pub(crate) async fn decide_proposal(
        &self,
        request: ApprovalRequest,
        approve: bool,
    ) -> Result<ApprovalRequest> {
        // Cleanup holds the gate, so it cannot remove the workspace between
        // opening the item and linking it, and two decisions cannot both open it.
        let _gate = self.pr_gate.lock().await;
        let request = self.pending_proposal(request).await?;
        let workspace = self.existing_workspace(&request.workspace_id).await?;
        let (request, message) = if approve {
            self.approve_proposal(request, workspace.as_ref()).await?
        } else {
            deny_proposal(request)
        };
        self.forget_proposal(request.key.clone()).await?;
        if let Some(workspace) = workspace {
            self.tell_workspace(&workspace.id, message).await;
        }
        Ok(request)
    }

    /// The stored request the user chose, still undecided.
    async fn pending_proposal(&self, chosen: ApprovalRequest) -> Result<ApprovalRequest> {
        let request = self
            .approval_request(chosen.id)
            .await?
            .filter(|request| request.key == chosen.key)
            .with_context(|| format!("approval request {} was already decided", chosen.id))?;
        ensure!(
            request.status == DecisionStatus::Pending,
            "approval request {} is {}: it no longer applies",
            request.id,
            request.status
        );
        Ok(request)
    }

    /// The workspace with this ID, unless it was removed.
    async fn existing_workspace(&self, id: &str) -> Result<Option<Workspace>> {
        let owned = id.to_owned();
        let exists = self
            .store
            .run(move |db| store::exists(db, "SELECT 1 FROM workspaces WHERE id=?1", [owned]))
            .await?;
        Ok(match exists {
            true => Some(self.workspace(id).await?),
            false => None,
        })
    }

    /// Open the proposal.
    async fn approve_proposal(
        &self,
        mut request: ApprovalRequest,
        workspace: Option<&Workspace>,
    ) -> Result<(ApprovalRequest, String)> {
        let (item, linked) = match request.target {
            Target::IssueOpen => self.open_proposed_issue(&request, workspace).await?,
            _ => bail!("approval request {} is not a proposal", request.id),
        };
        request.status = DecisionStatus::Approved;
        request.decided_at = Some(crate::time::unix_seconds());
        request.url = Some(item.url.clone());
        let linked = if linked { " and linked" } else { "" };
        let message = format!(
            "Approval {} to {} was approved: {} was opened{linked}.",
            request.id,
            request.subject(),
            item.url
        );
        Ok((request, message))
    }

    /// Open the proposed issue as the agent account, through the workspace
    /// while it exists and otherwise through the repository. Returns the issue
    /// and whether it was linked.
    async fn open_proposed_issue(
        &self,
        request: &ApprovalRequest,
        workspace: Option<&Workspace>,
    ) -> Result<(Item, bool)> {
        let proposed = request
            .proposed_issue()
            .context("the issue proposal has no issue")?;
        let (forge, account) = self.proposal_forge_account(request, workspace).await?;
        let item = forge.create_issue(&account, &proposed.issue).await?;
        let linked = match workspace {
            Some(workspace) => {
                let link = proposed.link && workspace.links.issue.is_none();
                // The issue exists, so a failed record or link only logs.
                match self
                    .keep_created(workspace, ItemKind::Issue, &item, link)
                    .await
                {
                    Ok((linked, _)) => linked,
                    Err(error) => {
                        crate::daemon::log::log!("proposed issue not recorded: {error:#}");
                        false
                    }
                }
            }
            None => false,
        };
        Ok((item, linked))
    }

    /// The forge and agent account a proposal acts through: its workspace's
    /// while it exists, otherwise its repository's.
    async fn proposal_forge_account(
        &self,
        request: &ApprovalRequest,
        workspace: Option<&Workspace>,
    ) -> Result<(ForgeRepo, Account)> {
        if let Some(workspace) = workspace {
            return self.workspace_forge_account(workspace).await;
        }
        let repository = request
            .repository_id
            .as_deref()
            .context("the workspace that made the proposal is gone")?;
        let path = self.repository(repository).await?.path;
        let remote = crate::repository::remote_url_from_path(&path)
            .await?
            .context("forge actions need an origin remote")?;
        let settings = self.repository_settings(repository).await?;
        let config = self.config();
        let account = self.agent_account_from(&config, &settings)?;
        Ok((ForgeRepo::parse(&remote)?, account))
    }
}

/// A pending request for `workspace` to open what `specification` describes.
fn proposal(
    workspace: &Workspace,
    target: Target,
    name: &str,
    specification: Specification,
) -> ApprovalRequest {
    ApprovalRequest::new(
        &workspace.id,
        target,
        name,
        specification,
        Lifetime::Lease,
        None,
    )
}

/// The denied proposal and the agent's message about it.
fn deny_proposal(mut request: ApprovalRequest) -> (ApprovalRequest, String) {
    request.status = DecisionStatus::Denied;
    request.decided_at = Some(crate::time::unix_seconds());
    let message = format!(
        "Approval {} to {} was denied. Do not open it another way.",
        request.id,
        request.subject(),
    );
    (request, message)
}
