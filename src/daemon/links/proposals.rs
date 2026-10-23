//! Issues and PRs a repository's `[approvals]` makes scoped callers propose
//! instead of opening: recording them, and opening, denying or merging them
//! as duplicates once the user decides.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{
    config::resolve::Effective,
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
        conventions,
        items::action::{Action, Edit, Sent},
        items::attach::{self, Attachment},
        items::create::{NewIssue, NewPull},
        items::item::{Item, ItemState, Opened},
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

/// The workspace branch was renamed or moved past the commit a PR proposal
/// was made for, or its pushed commit differs.
#[derive(Debug)]
struct Moved(Box<ApprovalRequest>);

impl std::fmt::Display for Moved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "approval request {} is outdated: the branch or its pushed commit changed; the agent was told to propose the PR again",
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
        mut issue: NewIssue,
        link: bool,
        proposer: Proposer,
    ) -> Result<Proposed> {
        self.keep_attachments(&workspace.id, &mut issue.body, &mut issue.attachments)
            .await?;
        let commit = crate::git::revision(&workspace.path, "HEAD", crate::git::run).await?;
        let specification = Specification::Issue(IssueSpecification {
            issue,
            link,
            branch: workspace.branch.clone(),
            commit: commit.trim().to_owned(),
            agent: proposer.agent,
            duplicates: Vec::new(),
            follow_up: None,
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
        mut pull: NewPull,
        commit: String,
        proposer: Proposer,
    ) -> Result<Proposed> {
        self.keep_attachments(&workspace.id, &mut pull.body, &mut pull.attachments)
            .await?;
        let specification = Specification::Pull(PullSpecification {
            pull,
            commit,
            agent: proposer.agent,
            follow_up: None,
        });
        let request = proposal(workspace, Target::PrOpen, proposals::PR, specification);
        self.record_proposal(workspace, request, proposals::propose_pull)
            .await
    }

    /// Copy the files a proposal attaches into Shoal's state and point
    /// `body` at the copies. The directory is named by the proposal's
    /// content, files included, so proposing it again reuses the copies and
    /// finds the same request, and copies never change. The caller holds the
    /// PR gate, which pruning takes too.
    async fn keep_attachments(
        &self,
        owner: &str,
        body: &mut String,
        attachments: &mut Vec<Attachment>,
    ) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        let root = self.paths.proposal_files();
        let (owner, text, originals) = (owner.to_owned(), body.clone(), attachments.clone());
        let copies =
            tokio::task::spawn_blocking(move || keep_copies(&root, &owner, &text, &originals))
                .await??;
        *body = attach::retarget(body, attachments, &copies);
        *attachments = copies;
        Ok(())
    }

    /// [`Self::prune_proposal_files`] for the cleanup sweep, which also
    /// catches proposals removed with their workspace or repository.
    pub(crate) async fn sweep_proposal_files(&self) -> Result<()> {
        let _gate = self.pr_gate.lock().await;
        self.remove_unattached_files().await
    }

    /// Delete copied files no pending proposal attaches any more. Failure
    /// only logs. The caller holds the PR gate.
    async fn prune_proposal_files(&self) {
        if let Err(error) = self.remove_unattached_files().await {
            crate::daemon::log::log!("proposal files not pruned: {error:#}");
        }
    }

    async fn remove_unattached_files(&self) -> Result<()> {
        let root = self.paths.proposal_files();
        let pending: Vec<_> = self
            .approval_requests(None)
            .await?
            .into_iter()
            .filter(|request| request.status == DecisionStatus::Pending)
            .collect();
        let kept: BTreeSet<_> = pending
            .iter()
            .flat_map(ApprovalRequest::attachments)
            .filter_map(|attachment| attachment.path.strip_prefix(&root).ok())
            .filter_map(|path| path.components().next())
            .map(|component| component.as_os_str().to_owned())
            .collect();
        tokio::task::spawn_blocking(move || remove_unattached(&root, &kept)).await?
    }

    /// Store the proposal with `record` and tell the user about a new one.
    async fn record_proposal(
        &self,
        workspace: &Workspace,
        request: ApprovalRequest,
        record: fn(&rusqlite::Transaction<'_>, ApprovalRequest) -> Result<Recorded>,
    ) -> Result<Proposed> {
        let (request, new) = self.store_proposal(request, record).await?;
        self.prune_proposal_files().await;
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
        self.prune_proposal_files().await;
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
        // Another decision may have saved edits, or opened the item, while
        // this one waited for the gate; the user reviewed the earlier text.
        ensure!(
            request.specification == chosen.specification && request.url == chosen.url,
            "approval request {} changed while it waited; review it again",
            request.id
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
            ensure!(
                request.url.is_none(),
                "approval request {} already opened its issue; approve it again without edits to finish",
                request.id
            );
            request.edit(decision);
            self.save_proposal(request.clone()).await?;
        }
        let (item, linked) = match request.target {
            Target::IssueOpen => self.open_proposed_issue(&mut request, workspace).await?,
            Target::PrOpen => {
                let workspace = workspace.context("the workspace that proposed the PR is gone")?;
                self.open_proposed_pull(&mut request, workspace).await?
            }
            _ => bail!("approval request {} is not a proposal", request.id),
        };
        let warnings = self.convention_warnings(&request, workspace, &item).await;
        request.warnings.extend(warnings);
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

    /// What the opened item leaves out of the repository's conventions, as
    /// `issue open` and `pr open` warn once the item exists. The labels the
    /// conventions require were checked when the item was proposed; refusing
    /// now would leave the user no way to add them. A failed lookup leaves
    /// its warnings out.
    async fn convention_warnings(
        &self,
        request: &ApprovalRequest,
        workspace: Option<&Workspace>,
        item: &Item,
    ) -> Vec<String> {
        let Ok((forge, account)) = self.proposal_forge_account(request, workspace).await else {
            return Vec::new();
        };
        let Ok((settings, path)) = self.proposal_settings(request, workspace).await else {
            return Vec::new();
        };
        let body = match &request.specification {
            Specification::Issue(issue) => Some(issue.issue.body.as_str()),
            Specification::Pull(pull) => Some(pull.pull.body.as_str()),
            _ => None,
        };
        let required = super::actions::required_labels(&settings, item.kind);
        conventions::warnings(&forge, &account, &path, item, body, required).await
    }

    /// The settings a proposal opens under and the checkout whose templates
    /// apply: its workspace's while it exists, otherwise its repository's.
    async fn proposal_settings(
        &self,
        request: &ApprovalRequest,
        workspace: Option<&Workspace>,
    ) -> Result<(Effective, PathBuf)> {
        if let Some(workspace) = workspace {
            let settings = self.workspace_settings(workspace).await?;
            return Ok((settings, workspace.path.clone()));
        }
        let repository = request
            .repository_id
            .as_deref()
            .context("the workspace that made the proposal is gone")?;
        let settings = self.repository_settings(repository).await?;
        Ok((settings, self.repository(repository).await?.path))
    }

    /// Open the proposed issue as the agent account, through the workspace
    /// while it exists and otherwise through the repository, and post the
    /// findings of its duplicates. The request keeps the issue once it is
    /// posted and the work left after it, so approving again after a failure
    /// finishes that work instead of opening another issue. Returns the issue
    /// and whether it was linked.
    async fn open_proposed_issue(
        &self,
        request: &mut ApprovalRequest,
        workspace: Option<&Workspace>,
    ) -> Result<(Item, bool)> {
        let (forge, account) = self.proposal_forge_account(request, workspace).await?;
        let linked = match request.url {
            Some(_) => false,
            None => {
                self.post_proposed_issue(request, workspace, &forge, &account)
                    .await?
            }
        };
        let item = self
            .finish_proposed(request, &forge, &account, ItemKind::Issue)
            .await?;
        self.post_findings(request, workspace, &forge, &account, &item)
            .await?;
        Ok((item, linked))
    }

    /// Post the issue, keep its URL and the edit that finishes it on the
    /// request, and record it as the workspace's. Returns whether it was linked.
    async fn post_proposed_issue(
        &self,
        request: &mut ApprovalRequest,
        workspace: Option<&Workspace>,
        forge: &ForgeRepo,
        account: &Account,
    ) -> Result<bool> {
        let proposed = request
            .proposed_issue()
            .context("the issue proposal has no issue")?
            .clone();
        let (number, follow_up) = forge.post_issue(account, &proposed.issue).await?;
        let url = forge.item_url(ItemKind::Issue, number)?;
        self.keep_posted(request, &url, follow_up).await?;
        let item = Item {
            kind: ItemKind::Issue,
            number,
            url,
            title: proposed.issue.title,
            state: ItemState::Open,
            labels: Vec::new(),
            pr: None,
        };
        Ok(self
            .record_proposed_issue(workspace, proposed.link, &item)
            .await)
    }

    /// Keep the posted item's URL and the edit that finishes it on the
    /// request, so approving again finishes it instead of posting another.
    async fn keep_posted(
        &self,
        request: &mut ApprovalRequest,
        url: &str,
        follow_up: Edit,
    ) -> Result<()> {
        request.url = Some(url.to_owned());
        request.set_follow_up(Some(follow_up));
        self.save_proposal(request.clone())
            .await
            .with_context(|| format!("{url} was opened, but recording it failed"))
    }

    /// Apply what posting the item left, such as its labels, and read it as
    /// the forge then reports it.
    async fn finish_proposed(
        &self,
        request: &mut ApprovalRequest,
        forge: &ForgeRepo,
        account: &Account,
        kind: ItemKind,
    ) -> Result<Item> {
        let url = request
            .url
            .clone()
            .context("the proposal has no opened item")?;
        let (number, _) = match kind {
            ItemKind::Issue => forge.issue(&url)?,
            ItemKind::Pr => forge.pull(&url)?,
        };
        let follow_up = request.follow_up().cloned().unwrap_or_default();
        let item = forge
            .finish_item(account, kind, number, &follow_up)
            .await
            .with_context(|| format!("{url}: approve again to retry"))?;
        if request.follow_up().is_some() {
            request.set_follow_up(None);
            self.save_proposal(request.clone()).await?;
        }
        Ok(item)
    }

    /// Comment the findings of the issue's duplicates, forgetting each once
    /// it is posted.
    async fn post_findings(
        &self,
        request: &mut ApprovalRequest,
        workspace: Option<&Workspace>,
        forge: &ForgeRepo,
        account: &Account,
        item: &Item,
    ) -> Result<()> {
        let target = super::actions::Target {
            kind: ItemKind::Issue,
            number: item.number,
            url: item.url.clone(),
        };
        while let Some(finding) = request.findings().first().cloned() {
            let unfinished = self
                .comment_as_proposer(workspace, forge, account, &target, comment(finding))
                .await
                .with_context(|| {
                    format!(
                        "{} was opened, but adding a duplicate failed; approve again to retry",
                        item.url
                    )
                })?;
            request.warnings.extend(unfinished);
            request.finding_posted();
            self.save_proposal(request.clone()).await?;
        }
        Ok(())
    }

    /// Record the opened issue as the workspace's and assign it when it was
    /// proposed with `link` and none is assigned. The issue exists, so a
    /// failure only logs. Returns whether it was linked.
    async fn record_proposed_issue(
        &self,
        workspace: Option<&Workspace>,
        link: bool,
        item: &Item,
    ) -> bool {
        let Some(workspace) = workspace else {
            return false;
        };
        let link = link && workspace.links.issue.is_none();
        match self
            .keep_created(workspace, ItemKind::Issue, item, link)
            .await
        {
            Ok((linked, _)) => linked,
            Err(error) => {
                crate::daemon::log::log!("proposed issue not recorded: {error:#}");
                false
            }
        }
    }

    /// Open the proposed PR, or link the branch's open PR, as the agent
    /// account. A branch that changed since the proposal outdates the request
    /// instead. The request keeps the PR once it is posted and the edit that
    /// finishes it, so approving again after a failure finishes that edit.
    async fn open_proposed_pull(
        &self,
        request: &mut ApprovalRequest,
        workspace: &Workspace,
    ) -> Result<(Item, bool)> {
        self.verify_worktree(workspace).await?;
        ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let (forge, account) = self.workspace_forge_account(workspace).await?;
        if request.url.is_none() {
            let proposed = request
                .proposed_pull()
                .context("the PR proposal has no PR")?
                .clone();
            if !pull_current(workspace, &proposed).await? {
                return Err(self.outdate_pull(request).await?.into());
            }
            if let Some(number) = forge.open_pull_for(&account, &workspace.branch).await? {
                return self
                    .link_open_pull(workspace, &forge, &account, number)
                    .await;
            }
            let (number, follow_up) = forge.post_pull(&account, &proposed.pull).await?;
            let url = forge.item_url(ItemKind::Pr, number)?;
            self.keep_posted(request, &url, follow_up).await?;
        } else {
            ensure_pull_head(&forge, &account, request, workspace).await?;
        }
        let item = self
            .finish_proposed(request, &forge, &account, ItemKind::Pr)
            .await?;
        let (linked, _) = self
            .keep_created(workspace, ItemKind::Pr, &item, true)
            .await?;
        Ok((item, linked))
    }

    /// Link the branch's PR, which someone opened since the proposal.
    async fn link_open_pull(
        &self,
        workspace: &Workspace,
        forge: &ForgeRepo,
        account: &Account,
        number: u64,
    ) -> Result<(Item, bool)> {
        let item = forge.read(account, ItemKind::Pr, number).await?.item;
        let watch = super::pr::Action::Watch {
            url: item.url.clone(),
        };
        self.set_pr_gated(&workspace.id, watch, super::pr::WatchCheck::Known)
            .await
            .with_context(|| format!("{} is open, but linking it failed", item.url))?;
        Ok((item, true))
    }

    /// Keep the request as outdated and tell the agent to propose again.
    async fn outdate_pull(&self, request: &ApprovalRequest) -> Result<Moved> {
        self.retire_approval(request.clone()).await?;
        let message = format!(
            "Approval {} to {} is outdated: the branch or its pushed commit changed. Run shoal pr open again to propose the PR at the current commit.",
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
                let (url, unfinished) = self.add_to_issue(&request, workspace, input).await?;
                request.warnings.extend(unfinished);
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
    ) -> Result<(String, Option<String>)> {
        let (forge, account) = self.proposal_forge_account(request, workspace).await?;
        let (number, url) = forge.issue(input)?;
        let target = super::actions::Target {
            kind: ItemKind::Issue,
            number,
            url: url.clone(),
        };
        let comment = comment(finding(request)?);
        let unfinished = self
            .comment_as_proposer(workspace, &forge, &account, &target, comment)
            .await?;
        Ok((url, unfinished))
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
    /// change while it exists. A failure after the comment reached the forge,
    /// such as attaching its files, is returned as a warning for the user
    /// instead, since posting the comment again would repeat it.
    async fn comment_as_proposer(
        &self,
        workspace: Option<&Workspace>,
        forge: &ForgeRepo,
        account: &Account,
        target: &super::actions::Target,
        comment: Action,
    ) -> Result<Option<String>> {
        let posted = match workspace {
            Some(workspace) => self
                .apply_own(&workspace.id, forge, account, target.clone(), &comment)
                .await
                .map(|_| ()),
            None => forge
                .apply(account, ItemKind::Issue, target.number, &comment)
                .await
                .map(|_| ()),
        };
        match posted {
            Ok(()) => Ok(None),
            Err(error) if error.downcast_ref::<Sent>().is_some() => {
                Ok(Some(format!("{}: {error:#}", target.url)))
            }
            Err(error) => Err(error),
        }
    }

    /// The forge and agent account a proposal acts through: its workspace's
    /// while it exists, otherwise its repository's.
    async fn proposal_forge_account(
        &self,
        request: &ApprovalRequest,
        workspace: Option<&Workspace>,
    ) -> Result<(ForgeRepo, Account)> {
        if let Some(workspace) = workspace {
            // The worktree may have been replaced while the proposal waited.
            self.verify_worktree(workspace).await?;
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
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self.repository_settings_from(&config, repository).await?;
        let account = self.agent_account_from(&config, &settings)?;
        Ok((ForgeRepo::parse(&remote)?, account))
    }
}

/// Refuse to finish a PR an earlier approval posted once the workspace's
/// branch is no longer its head, as after a rename: linking requires it.
async fn ensure_pull_head(
    forge: &ForgeRepo,
    account: &Account,
    request: &ApprovalRequest,
    workspace: &Workspace,
) -> Result<()> {
    let url = request
        .url
        .as_deref()
        .context("the proposal has no opened PR")?;
    let (number, _) = forge.pull(url)?;
    let item = forge.read(account, ItemKind::Pr, number).await?.item;
    ensure!(
        item.pr
            .as_ref()
            .is_some_and(|pr| pr.head == workspace.branch),
        "{url} does not have the workspace branch {} as its head",
        workspace.branch
    );
    Ok(())
}

/// Whether the workspace still has the proposed branch at the proposed
/// commit, both locally and where `pr open` pushed it, so approval opens
/// only what the user saw.
async fn pull_current(workspace: &Workspace, proposed: &PullSpecification) -> Result<bool> {
    if proposed.pull.head != workspace.branch
        || super::pr::current_head(workspace).await? != proposed.commit
    {
        return Ok(false);
    }
    let pushed = crate::git::pushed_commit(&workspace.path, "origin", &workspace.branch).await?;
    Ok(pushed.as_deref() == Some(proposed.commit.as_str()))
}

/// The copies of `attachments` below `root`, in a directory named by the
/// proposal's content. An existing directory is reused unchanged; a new one is
/// filled under a temporary name first, so a partial copy is never reused.
fn keep_copies(
    root: &Path,
    owner: &str,
    body: &str,
    attachments: &[Attachment],
) -> Result<Vec<Attachment>> {
    let directory = root.join(content_key(owner, body, attachments)?);
    if !directory.exists() {
        let staging = root.join(format!(".copying-{}", uuid::Uuid::new_v4()));
        copy_attachments(&staging, attachments)?;
        if let Err(error) = std::fs::rename(&staging, &directory) {
            std::fs::remove_dir_all(&staging).ok();
            ensure!(directory.exists(), "keep {}: {error}", directory.display());
        }
    }
    Ok(attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| Attachment {
            path: copy_path(&directory, index, attachment),
            alt: attachment.alt.clone(),
        })
        .collect())
}

/// A name for the files of a proposal with this owner, body and attachments,
/// their contents included.
fn content_key(owner: &str, body: &str, attachments: &[Attachment]) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in [owner, body] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    for attachment in attachments {
        hasher.update(attachment.path.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(attachment.alt.as_deref().unwrap_or_default().as_bytes());
        hasher.update([0]);
        let contents = std::fs::read(&attachment.path)
            .with_context(|| format!("read {}", attachment.path.display()))?;
        hasher.update(Sha256::digest(&contents));
    }
    Ok(hasher.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Where the attachment at `index` is copied below `directory`: its own
/// directory, keeping the file name, which names the file where it is shown.
fn copy_path(directory: &Path, index: usize, attachment: &Attachment) -> PathBuf {
    let name = attachment.path.file_name().unwrap_or_default();
    directory.join(index.to_string()).join(name)
}

/// Copy each attachment to its [`copy_path`] below `directory`.
fn copy_attachments(directory: &Path, attachments: &[Attachment]) -> Result<()> {
    for (index, attachment) in attachments.iter().enumerate() {
        let target = copy_path(directory, index, attachment);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(&attachment.path, &target)
            .with_context(|| format!("copy {}", attachment.path.display()))?;
    }
    Ok(())
}

/// Remove the directories below `root` whose names `kept` leaves out.
fn remove_unattached(root: &Path, kept: &BTreeSet<OsString>) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        if !kept.contains(&entry.file_name()) {
            std::fs::remove_dir_all(entry.path())
                .with_context(|| format!("remove {}", entry.path().display()))?;
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon::approvals::proposals::PR,
        test_support::{manager, repository},
    };

    #[tokio::test]
    async fn the_sweep_keeps_only_files_pending_proposals_attach() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        let files = manager.paths.proposal_files();
        let kept = files.join("kept/0/shot.png");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, "png").unwrap();
        std::fs::create_dir_all(files.join("stale/0")).unwrap();
        let pull = NewPull {
            attachments: vec![Attachment {
                path: kept.clone(),
                alt: None,
            }],
            ..NewPull::default()
        };
        let specification = Specification::Pull(PullSpecification {
            pull,
            commit: "head".into(),
            agent: None,
            follow_up: None,
        });
        let request = proposal(&workspace, Target::PrOpen, PR, specification);
        manager
            .store_proposal(request, proposals::propose_pull)
            .await
            .unwrap();

        manager.sweep_proposal_files().await.unwrap();
        assert!(kept.exists());
        assert!(!files.join("stale").exists());

        // Removing the workspace takes its PR proposal, and the next sweep
        // its files.
        manager
            .store
            .run(|db| Ok(db.execute("DELETE FROM workspaces", [])?))
            .await
            .unwrap();
        manager.sweep_proposal_files().await.unwrap();
        assert!(!files.join("kept").exists());
    }

    #[tokio::test]
    async fn a_decision_refuses_a_proposal_changed_while_it_waited() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        let issue = NewIssue {
            title: "Flaky".into(),
            ..NewIssue::default()
        };
        let specification = Specification::Issue(IssueSpecification {
            issue,
            link: false,
            branch: "topic".into(),
            commit: "head".into(),
            agent: None,
            duplicates: Vec::new(),
            follow_up: None,
        });
        let request = proposal(
            &workspace,
            Target::IssueOpen,
            proposals::ISSUE,
            specification,
        );
        let (chosen, _) = manager
            .store_proposal(request, proposals::propose_issue)
            .await
            .unwrap();
        assert!(manager.pending_proposal(chosen.clone()).await.is_ok());

        // Another approval saves its edits before this decision takes the gate.
        let mut edited = chosen.clone();
        edited.edit(&ProposalDecision {
            title: Some("Flaky test".into()),
            ..ProposalDecision::default()
        });
        manager.save_proposal(edited).await.unwrap();
        let error = manager.pending_proposal(chosen).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("changed while it waited"),
            "{error:#}"
        );
    }
}
