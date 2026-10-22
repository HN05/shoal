//! Issues and PRs a repository's `[approvals]` makes scoped callers propose
//! instead of opening: recording them, and opening, denying or merging them
//! as duplicates once the user decides.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    daemon::{
        approvals::{
            ApprovalRequest, DecisionStatus, Lifetime, Specification, Target,
            proposals::{
                self, Duplicate, Finding, IssueSpecification, ProposalDecision, PullSpecification,
                Recorded,
            },
        },
        workspace::Manager,
    },
    forge::{
        ForgeRepo,
        account::Account,
        items::action::Action,
        items::create::{NewIssue, NewPull},
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

/// The branch moved past the commit a PR proposal was made for.
#[derive(Debug)]
struct Moved(Box<ApprovalRequest>);

impl std::fmt::Display for Moved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "approval request {} is outdated: the branch has newer commits; the agent was told to propose the PR again",
            self.0.id
        )
    }
}

impl std::error::Error for Moved {}

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
            duplicates: Vec::new(),
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

    /// Record `pull` at the branch's `commit` for the user's approval instead
    /// of opening it.
    pub(super) async fn propose_pull(
        &self,
        workspace: &Workspace,
        pull: NewPull,
        commit: String,
        proposer: Proposer,
    ) -> Result<Proposed> {
        let specification = Specification::Pull(PullSpecification {
            pull,
            commit,
            agent: proposer.agent,
        });
        let request = proposal(workspace, Target::PrOpen, proposals::PR, specification);
        self.record_proposal(workspace, request, proposals::propose_pull)
            .await
    }

    /// Store the proposal with `record` and tell the user about a new one.
    async fn record_proposal(
        &self,
        workspace: &Workspace,
        request: ApprovalRequest,
        record: fn(&rusqlite::Transaction<'_>, ApprovalRequest) -> Result<Recorded>,
    ) -> Result<Proposed> {
        let (request, new) = self.store_proposal(request, record).await?;
        if new {
            self.notify_approval(&workspace.name, &request).await;
        }
        Ok(Proposed {
            proposed: Box::new(request),
        })
    }

    /// Open, deny or merge as a duplicate the proposal the user chose, then
    /// forget it and tell the proposing workspace's agents.
    pub(crate) async fn decide_proposal(
        &self,
        request: ApprovalRequest,
        approve: bool,
        decision: ProposalDecision,
    ) -> Result<ApprovalRequest> {
        decision.validate(&request.target, approve)?;
        // Cleanup holds the gate, so it cannot remove the workspace between
        // opening the item and linking it, and two decisions cannot both open it.
        let _gate = self.pr_gate.lock().await;
        let request = self.pending_proposal(request).await?;
        let workspace = self.find_workspace(&request.workspace_id).await?;
        let (request, message) = if approve {
            self.approve_proposal(request, workspace.as_ref(), &decision)
                .await?
        } else {
            self.deny_proposal(request, workspace.as_ref(), &decision)
                .await?
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

    /// Open the proposal with the user's edits, which are kept when opening fails.
    async fn approve_proposal(
        &self,
        mut request: ApprovalRequest,
        workspace: Option<&Workspace>,
        decision: &ProposalDecision,
    ) -> Result<(ApprovalRequest, String)> {
        if decision.title.is_some() || decision.body.is_some() {
            request.edit(decision);
            self.save_proposal(request.clone()).await?;
        }
        let (item, linked) = match request.target {
            Target::IssueOpen => self.open_proposed_issue(&request, workspace).await?,
            Target::PrOpen => {
                let workspace = workspace.context("the workspace that proposed the PR is gone")?;
                self.open_proposed_pull(&request, workspace).await?
            }
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
    /// while it exists and otherwise through the repository, and post the
    /// findings of its duplicates. Returns the issue and whether it was linked.
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
        let target = super::actions::Target {
            kind: ItemKind::Issue,
            number: item.number,
            url: item.url.clone(),
        };
        for finding in &proposed.duplicates {
            let comment = comment(finding.clone());
            self.comment_as_proposer(workspace, &forge, &account, &target, comment)
                .await
                .with_context(|| {
                    format!("{} was opened, but adding a duplicate failed", item.url)
                })?;
        }
        Ok((item, linked))
    }

    /// Open the proposed PR, or link the branch's open PR, as the agent
    /// account. A branch with newer commits outdates the request instead.
    async fn open_proposed_pull(
        &self,
        request: &ApprovalRequest,
        workspace: &Workspace,
    ) -> Result<(Item, bool)> {
        let proposed = request
            .proposed_pull()
            .context("the PR proposal has no PR")?;
        self.verify_worktree(workspace).await?;
        ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        if super::pr::current_head(workspace).await? != proposed.commit {
            return Err(self.outdate_pull(request).await?.into());
        }
        let (forge, account) = self.workspace_forge_account(workspace).await?;
        let item = match forge.open_pull_for(&account, &workspace.branch).await? {
            Some(number) => {
                let item = forge.read(&account, ItemKind::Pr, number).await?.item;
                let watch = super::pr::Action::Watch {
                    url: item.url.clone(),
                };
                self.set_pr_gated(&workspace.id, watch, super::pr::WatchCheck::Known)
                    .await
                    .with_context(|| format!("{} is open, but linking it failed", item.url))?;
                return Ok((item, true));
            }
            None => forge.create_pull(&account, &proposed.pull).await?,
        };
        let (linked, _) = self
            .keep_created(workspace, ItemKind::Pr, &item, true)
            .await?;
        Ok((item, linked))
    }

    /// Keep the request as outdated and tell the agent to propose again.
    async fn outdate_pull(&self, request: &ApprovalRequest) -> Result<Moved> {
        self.retire_approval(request.clone()).await?;
        let message = format!(
            "Approval {} to {} is outdated: the branch has newer commits. Run shoal pr open again to propose the PR at the current commit.",
            request.id,
            request.subject()
        );
        self.tell_workspace(&request.workspace_id, message).await;
        Ok(Moved(Box::new(request.clone())))
    }

    /// Deny the proposal, adding an issue's finding to what it duplicates.
    async fn deny_proposal(
        &self,
        mut request: ApprovalRequest,
        workspace: Option<&Workspace>,
        decision: &ProposalDecision,
    ) -> Result<(ApprovalRequest, String)> {
        let outcome = match &decision.duplicate_of {
            Some(Duplicate::Issue(input)) => {
                let url = self.add_to_issue(&request, workspace, input).await?;
                request.url = Some(url.clone());
                format!(" as a duplicate of {url}; its text was added there as a comment")
            }
            Some(Duplicate::Approval(number)) => {
                self.add_to_proposal(&request, *number).await?;
                format!(
                    " as a duplicate of approval {number}; its text is added as a comment if that issue is opened"
                )
            }
            None => String::new(),
        };
        request.status = DecisionStatus::Denied;
        request.decided_at = Some(crate::time::unix_seconds());
        let reason = match &decision.reason {
            Some(reason) => format!(" Reason: {reason}"),
            None => String::new(),
        };
        let message = format!(
            "Approval {} to {} was denied{outcome}. Do not open it another way.{reason}",
            request.id,
            request.subject(),
        );
        Ok((request, message))
    }

    /// Comment the proposal's finding on the existing issue `input` names,
    /// returning its URL.
    async fn add_to_issue(
        &self,
        request: &ApprovalRequest,
        workspace: Option<&Workspace>,
        input: &str,
    ) -> Result<String> {
        let (forge, account) = self.proposal_forge_account(request, workspace).await?;
        let (number, url) = forge.issue(input)?;
        let target = super::actions::Target {
            kind: ItemKind::Issue,
            number,
            url: url.clone(),
        };
        let comment = comment(finding(request)?);
        self.comment_as_proposer(workspace, &forge, &account, &target, comment)
            .await?;
        Ok(url)
    }

    /// Keep the proposal's finding with the pending issue proposal `number`,
    /// which posts it once it is opened.
    async fn add_to_proposal(&self, request: &ApprovalRequest, number: i64) -> Result<()> {
        let mut original = self
            .approval_request(number)
            .await?
            .with_context(|| format!("unknown approval request {number}"))?;
        ensure!(
            original.key != request.key,
            "a proposal cannot duplicate itself"
        );
        ensure!(
            original.target == Target::IssueOpen
                && original.status == DecisionStatus::Pending
                && original.repository_id == request.repository_id,
            "approval request {number} is not a pending issue proposal in the same repository"
        );
        let finding = finding(request)?;
        match &mut original.specification {
            Specification::Issue(issue) => issue.duplicates.push(finding),
            _ => bail!("approval request {number} has no proposed issue"),
        }
        self.save_proposal(original).await
    }

    /// Post `comment` as the agent account, recorded as the workspace's own
    /// change while it exists.
    async fn comment_as_proposer(
        &self,
        workspace: Option<&Workspace>,
        forge: &ForgeRepo,
        account: &Account,
        target: &super::actions::Target,
        comment: Action,
    ) -> Result<()> {
        match workspace {
            Some(workspace) => {
                self.apply_own(&workspace.id, forge, account, target.clone(), &comment)
                    .await?;
            }
            None => {
                forge
                    .apply(account, ItemKind::Issue, target.number, &comment)
                    .await?;
            }
        }
        Ok(())
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

/// A duplicate proposal's title, description and attachments, for what it
/// duplicates.
fn finding(request: &ApprovalRequest) -> Result<Finding> {
    let proposed = request
        .proposed_issue()
        .context("only an issue proposal can be a duplicate")?;
    let mut text = format!(
        "Workspace {} proposed this as a separate issue, which was marked as a duplicate.\n\n### {}\n",
        request.workspace, proposed.issue.title
    );
    if !proposed.issue.body.trim().is_empty() {
        text.push('\n');
        text.push_str(proposed.issue.body.trim_end());
        text.push('\n');
    }
    Ok(Finding {
        body: text,
        attachments: proposed.issue.attachments.clone(),
    })
}

/// The comment that adds `finding` to what it duplicates.
fn comment(finding: Finding) -> Action {
    Action::Comment {
        body: finding.body,
        attachments: finding.attachments,
    }
}
