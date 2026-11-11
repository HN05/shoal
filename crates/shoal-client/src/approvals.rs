//! Approval requests: what a workspace's agent asks the user, or a coordinating
//! agent, to allow.
use crate::repository_ids::RepositoryId;
use crate::space_ids::SpaceId;
use crate::space_names::SpaceName;
use std::{collections::BTreeMap, fmt, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::space_names::SpaceSelector;
use crate::{
    config::repo::ConflictPolicy,
    connection::{Daemon, request},
    forge::items::{
        action::Edit,
        attach::Attachment,
        create::{NewIssue, NewPull},
    },
    item_numbers::ItemNumber,
    links::{BranchPush, PushOutcome},
    protocol::ApprovalMethod,
    resources::{Definition, LockMode, RepositoryView, Scope},
    sim::Profile,
    state::{states, text_key},
};

states!(
    #[derive(Default)]
    Lifetime {
        #[default]
        Lease => "lease",
        Workspace => "workspace",
    }
);

states!(DecisionStatus {
    Pending => "pending",
    Approved => "approved",
    Denied => "denied",
    Outdated => "outdated",
    /// The requesting agent no longer needed it.
    Withdrawn => "withdrawn",
});

/// What a decision authorizes; one active request per workspace, target and
/// name. Keyed as `resource/<scope>/<pool>`, `port/<name>`, `simulator`,
/// `secret/<name>`, `hold/<name>`, `merge`, `unmerge`, `issue_open`, `pr_open`
/// or `push`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Resource {
        scope: Scope,
        pool: String,
    },
    Port(String),
    Simulator,
    /// Fetching a configured secret; the daemon records the grant, never the value.
    Secret(String),
    /// Merging a workspace branch into the default branch.
    Merge,
    /// Undoing a workspace branch's local merge into the default branch.
    Unmerge,
    /// Opening an issue a scoped caller proposed; the repository owns the
    /// request, so it outlives the workspace.
    IssueOpen,
    /// Opening a PR for the workspace branch at one commit.
    PrOpen,
    /// Pushing one ref to a remote at one commit, named by the ref.
    Push,
    /// A hold an agent asks to place on its own workspace, which approval
    /// places.
    Hold(String),
}

/// The name of a workspace's single merge request, and of its single
/// unmerge request.
pub const MERGE: &str = "branch";

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource { scope, pool } => write!(f, "resource/{scope}/{pool}"),
            Self::Port(name) => write!(f, "port/{name}"),
            Self::Simulator => f.write_str("simulator"),
            Self::Secret(name) => write!(f, "secret/{name}"),
            Self::Merge => f.write_str("merge"),
            Self::Unmerge => f.write_str("unmerge"),
            Self::IssueOpen => f.write_str("issue_open"),
            Self::PrOpen => f.write_str("pr_open"),
            Self::Push => f.write_str("push"),
            Self::Hold(name) => write!(f, "hold/{name}"),
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        match text {
            "simulator" => return Ok(Self::Simulator),
            "merge" => return Ok(Self::Merge),
            "unmerge" => return Ok(Self::Unmerge),
            "issue_open" => return Ok(Self::IssueOpen),
            "pr_open" => return Ok(Self::PrOpen),
            "push" => return Ok(Self::Push),
            _ => {}
        }
        if let Some(name) = text.strip_prefix("port/")
            && !name.is_empty()
        {
            return Ok(Self::Port(name.into()));
        }
        if let Some(name) = text.strip_prefix("secret/")
            && !name.is_empty()
        {
            return Ok(Self::Secret(name.into()));
        }
        if let Some(name) = text.strip_prefix("hold/")
            && !name.is_empty()
        {
            return Ok(Self::Hold(name.into()));
        }
        // The scope may itself contain a slash; pool names cannot.
        if let Some((scope, pool)) = text
            .strip_prefix("resource/")
            .and_then(|rest| rest.rsplit_once('/'))
            && !pool.is_empty()
        {
            return Ok(Self::Resource {
                scope: scope.parse()?,
                pool: pool.into(),
            });
        }
        bail!("invalid approval target: {text}")
    }
}

text_key!(Target);

impl Target {
    /// Whether the request proposes a forge write the user may edit before
    /// approving, rather than granting access.
    pub fn is_proposal(&self) -> bool {
        matches!(self, Self::IssueOpen | Self::PrOpen | Self::Push)
    }

    /// Whether a coordinator decides the request only while it holds a grant
    /// for the same thing. A hold grants nothing a coordinator could hold, so
    /// the nearest one decides it.
    pub fn needs_grant(&self) -> bool {
        !matches!(self, Self::Hold(_))
    }
}

/// The effective allocation settings a decision binds; a later request with
/// different settings cannot reuse the grant. Each kind keeps the JSON shape
/// its allocator has always recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Specification {
    Resource(ResourceSpecification),
    Port(PortSpecification),
    Simulator(SimulatorSpecification),
    Secret(SecretSpecification),
    Merge(MergeSpecification),
    Unmerge(UnmergeSpecification),
    Issue(IssueSpecification),
    Pull(PullSpecification),
    Push(PushSpecification),
    Hold(HoldSpecification),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpecification {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<RepositoryView>,
    pub definition: Definition,
    /// The pool member the decision selects.
    pub member: String,
    pub mode: LockMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSpecification {
    pub env: String,
    pub on_conflict: ConflictPolicy,
    pub preferred: Option<u16>,
    /// Automatic range as `[start, end]`.
    pub range: [u16; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimulatorSpecification {
    pub clean: bool,
    pub profile: Profile,
}

/// A merge decision covers the branch at one commit, whether it pushes the
/// default branch afterwards and what else that push publishes, and the
/// sensitive files it warned about; later commits or another warning need a
/// new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeSpecification {
    pub branch: String,
    pub head: String,
    /// Where the merge pushes the default branch afterwards: the remote and
    /// the ref there, so another destination needs another decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<crate::model::MergePush>,
    /// The default branch's commit when the push publishes commits of it
    /// besides the merge's, so other local commits need another decision;
    /// none otherwise, so a sync of the default branch needs none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpushed: Option<String>,
    /// The sensitive files the merge brings to a default branch that changed
    /// them too, so a warning recorded after the decision needs another.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_on_both_sides: Vec<String>,
}

/// An unmerge decision covers undoing one recorded merge of the branch: the
/// commit that merged and the default branch's commit it left; another merge
/// needs a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmergeSpecification {
    /// The record of the merge it undoes, which tells apart merges of the
    /// same commit.
    pub merge: i64,
    pub branch: String,
    pub source: String,
    pub result: String,
}

/// A secret grant covers the configured source; changing the command or the
/// tools it serves needs a new decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretSpecification {
    pub command: Vec<String>,
    pub tools: Vec<String>,
}

/// A hold covers its name on the requesting workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldSpecification {
    pub hold: String,
}

impl fmt::Display for Specification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&serde_json::to_string(self).map_err(|_| fmt::Error)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The number the user decides it by, its space's and its own; reused
    /// once the request is deleted.
    #[serde(flatten)]
    pub number: ItemNumber,
    /// Stable identity behind the user-facing number, carried through displayed snapshots.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The space that made the request, a workspace or a chat.
    pub space_id: SpaceId,
    /// The space's name, which the daemon records with the request; none
    /// before it does, and in a record made before requests kept it.
    #[serde(default)]
    pub space: Option<SpaceName>,
    /// Whether the space is gone, which a proposed issue outlives.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub space_removed: bool,
    /// The repository that owns a proposed issue, which outlives the
    /// space that proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<RepositoryId>,
    pub target: Target,
    pub name: String,
    pub specification: Specification,
    pub lifetime: Lifetime,
    pub reason: String,
    pub status: DecisionStatus,
    pub created_at: crate::time::UnixTime,
    pub decided_at: Option<crate::time::UnixTime>,
    /// False for workspace grants retained after release.
    pub active: bool,
    /// Whether the request went up the workspace tree to find its decider.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub routed: bool,
    /// The space whose coordinating agent decides the request, the nearest
    /// above the requester with an agent; the user decides without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider: Option<SpaceId>,
    /// The issue or PR an approved proposal opened, or the issue a denied
    /// one duplicates; decided proposals are no longer stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What deciding a proposal could not finish, such as attaching the
    /// files of a duplicate's comment that was posted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The operation pushing what the user approved, which decides the
    /// request once the push is done; approving again waits until it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
}

impl ApprovalRequest {
    /// The proposed issue, for an issue proposal.
    pub fn proposed_issue(&self) -> Option<&IssueSpecification> {
        match &self.specification {
            Specification::Issue(issue) => Some(issue),
            _ => None,
        }
    }

    /// The proposed PR, for a PR proposal.
    pub fn proposed_pull(&self) -> Option<&PullSpecification> {
        match &self.specification {
            Specification::Pull(pull) => Some(pull),
            _ => None,
        }
    }

    /// The push approval runs: a push request's, or a PR proposal's that
    /// pushes the branch first.
    pub fn pending_push(&self) -> Option<&PushSpecification> {
        match &self.specification {
            Specification::Push(push) => Some(push),
            Specification::Pull(pull) => pull.push.as_ref(),
            _ => None,
        }
    }

    /// The files the proposal attaches, its duplicates' included.
    pub fn attachments(&self) -> Vec<&Attachment> {
        match &self.specification {
            Specification::Issue(issue) => issue
                .issue
                .attachments
                .iter()
                .chain(issue.duplicates.iter().flat_map(|d| &d.attachments))
                .collect(),
            Specification::Pull(pull) => pull.pull.attachments.iter().collect(),
            _ => Vec::new(),
        }
    }

    /// The findings of duplicates an issue proposal has yet to post.
    pub fn findings(&self) -> &[Finding] {
        self.proposed_issue()
            .map_or(&[], |issue| issue.duplicates.as_slice())
    }

    /// What is left to do to an item approval posted.
    pub fn follow_up(&self) -> Option<&Edit> {
        match &self.specification {
            Specification::Issue(issue) => issue.follow_up.as_ref(),
            Specification::Pull(pull) => pull.follow_up.as_ref(),
            _ => None,
        }
    }

    pub fn set_follow_up(&mut self, follow_up: Option<Edit>) {
        match &mut self.specification {
            Specification::Issue(issue) => issue.follow_up = follow_up,
            Specification::Pull(pull) => pull.follow_up = follow_up,
            _ => {}
        }
    }

    /// Forget the first finding once it is posted.
    pub fn finding_posted(&mut self) {
        if let Specification::Issue(issue) = &mut self.specification
            && !issue.duplicates.is_empty()
        {
            issue.duplicates.remove(0);
        }
    }

    /// Apply the user's edits to the proposed title and description.
    pub fn edit(&mut self, decision: &ProposalDecision) {
        let (title, body) = match &mut self.specification {
            Specification::Issue(issue) => (&mut issue.issue.title, &mut issue.issue.body),
            Specification::Pull(pull) => (&mut pull.pull.title, &mut pull.pull.body),
            _ => return,
        };
        if let Some(edited) = &decision.title {
            edited.clone_into(title);
        }
        if let Some(edited) = &decision.body {
            edited.clone_into(body);
        }
    }
}

/// The name of a workspace's single PR proposal.
pub const PR: &str = "branch";

/// The name every issue proposal shares; the repository holds any number.
pub const ISSUE: &str = "issue";

/// What `shoal issue open` was told, and where it was proposed from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueSpecification {
    pub issue: NewIssue,
    /// The URL of the repository the issue opens in, when it is not the
    /// workspace's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Whether approval also assigns the issue to the space.
    pub link: bool,
    /// The proposing workspace's branch; `None` for one at a commit and for
    /// a chat.
    pub branch: Option<String>,
    /// The proposing workspace's commit; `None` for a chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
    /// Findings of proposals the user marked as duplicates of this one,
    /// posted as comments once it is opened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<Finding>,
    /// What is left to do to the issue once approval posted it, such as
    /// Forgejo's labels, until it is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<Edit>,
}

/// A duplicate proposal's text and files, which become a comment on what it
/// duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// The PR `shoal pr open` would open, bound to the branch commit the user
/// approves; a newer commit outdates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullSpecification {
    pub pull: NewPull,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
    /// What is left to do to the PR once approval posted it, such as its
    /// labels and reviewers, until it is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<Edit>,
    /// The push of the branch that approval runs before opening the PR,
    /// when the `push` permission held it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushSpecification>,
}

/// A push of one ref that the `push` permission held back, which approval
/// runs as recorded: the commit, not whatever the branch has by then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushSpecification {
    /// The remote the push goes to, by name or URL.
    pub remote: String,
    /// The one push URL of `remote` the push goes to, when the remote has
    /// several and Git pushed to each on its own; none pushes wherever the
    /// remote does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The ref it updates there, such as `refs/heads/topic`.
    pub destination: String,
    /// The object it pushes; none deletes the ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// For a forced push or a deletion, the commit the remote ref had when
    /// it was requested, which it must still have, or empty when the ref
    /// must not exist; a push without one only fast-forwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<crate::agent::Agent>,
}

impl PushSpecification {
    /// Whether `other` makes the same change to the same remote ref,
    /// whichever agent asked for it.
    pub fn is_same_push(&self, other: &Self) -> bool {
        self.remote == other.remote
            && self.url == other.url
            && self.destination == other.destination
            && self.commit == other.commit
            && self.replaces == other.replaces
    }

    /// Where the push goes: its one push URL, else the remote.
    pub fn target(&self) -> &str {
        self.url.as_deref().unwrap_or(&self.remote)
    }

    /// The name of its request, one per workspace: the ref, and the push
    /// URL when the push goes to only one of the remote's.
    pub fn request_name(&self) -> String {
        match &self.url {
            Some(url) => format!("{} {url}", self.destination),
            None => self.destination.clone(),
        }
    }

    /// The ref's short name, such as `topic` for `refs/heads/topic`.
    pub fn short_destination(&self) -> &str {
        self.destination
            .strip_prefix("refs/heads/")
            .unwrap_or(&self.destination)
    }

    /// What the push does, as the user is asked to approve it.
    pub fn subject(&self) -> String {
        let (destination, remote) = (self.short_destination(), self.target());
        match (&self.commit, self.replaces.as_deref()) {
            (None, _) => format!("delete {destination} on {remote}"),
            (Some(commit), None | Some("")) => {
                format!("push {destination} at {commit} to {remote}")
            }
            (Some(commit), Some(_)) => {
                format!("force-push {destination} at {commit} to {remote}")
            }
        }
    }
}

/// What the user decides about a proposal besides yes or no.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalDecision {
    /// Replaces the proposed title on approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Replaces the proposed description on approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Tells the agent why its proposal was denied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Denies an issue proposal as a duplicate and adds its finding there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<Duplicate>,
}

/// What a denied issue proposal duplicates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Duplicate {
    /// An issue number or URL in the same repository.
    Issue(String),
    /// Another pending issue proposal, by its number.
    Approval(ItemNumber),
}

impl ProposalDecision {
    /// Refuse details that do not apply to this decision of `target`.
    pub fn validate(&self, target: &Target, approve: bool) -> Result<()> {
        self.check(approve)?;
        ensure!(
            self.duplicate_of.is_none() || *target == Target::IssueOpen,
            "only an issue proposal can be a duplicate"
        );
        ensure!(
            *target != Target::Push || (self.title.is_none() && self.body.is_none()),
            "a push has no title or body to edit"
        );
        Ok(())
    }

    /// Refuse details that do not apply to an approval or a denial of any
    /// request, before the request is known.
    pub fn check(&self, approve: bool) -> Result<()> {
        if approve {
            ensure!(
                self.reason.is_none() && self.duplicate_of.is_none(),
                "a reason or duplicate applies only when denying"
            );
            ensure!(
                self.title
                    .as_ref()
                    .is_none_or(|title| !title.trim().is_empty()),
                "the title cannot be empty"
            );
        } else {
            ensure!(
                self.title.is_none() && self.body.is_none(),
                "a title or body applies only when approving"
            );
        }
        crate::validate::reason("denial", self.reason.as_deref())
    }
}

impl ApprovalRequest {
    /// The name of the space that made it; empty when the record has none.
    pub fn space_name(&self) -> &str {
        self.space.as_deref().unwrap_or_default()
    }

    /// What the user is asked to decide: the access and how long it lasts,
    /// the branch and commit to merge, the issue or PR to open, or the hold
    /// to place.
    pub fn subject(&self) -> String {
        match (&self.target, &self.specification) {
            (Target::Merge, Specification::Merge(merge)) => match &merge.push {
                Some(push) => format!(
                    "merge {} at {} and push to {}",
                    merge.branch,
                    merge.head,
                    push.remote_branch()
                ),
                None => format!("merge {} at {}", merge.branch, merge.head),
            },
            (Target::Unmerge, Specification::Unmerge(unmerge)) => format!(
                "undo the merge of {} at {} ({})",
                unmerge.branch, unmerge.source, unmerge.result
            ),
            (Target::IssueOpen, Specification::Issue(issue)) => {
                let title = &issue.issue.title;
                match &issue.repository {
                    Some(repository) => format!("open issue \"{title}\" in {repository}"),
                    None => format!("open issue \"{title}\""),
                }
            }
            (Target::Hold(name), _) => format!("hold the workspace as {name}"),
            (Target::PrOpen, Specification::Pull(pull)) => {
                let push = match &pull.push {
                    Some(_) => format!("push {} and ", pull.pull.head),
                    None => String::new(),
                };
                format!(
                    "{push}open PR \"{}\" from {} at {}",
                    pull.pull.title, pull.pull.head, pull.commit
                )
            }
            (Target::Push, Specification::Push(push)) => push.subject(),
            _ => format!("{} ({})", self.access(), self.lifetime),
        }
    }

    /// The kind and name of what the request grants access to, and the
    /// lease name when the agent chose its own.
    fn access(&self) -> String {
        let (kind, name) = match &self.target {
            Target::Resource { pool, .. } => ("resource", Some(pool)),
            Target::Port(name) => ("port", Some(name)),
            Target::Secret(name) => ("secret", Some(name)),
            Target::Simulator => ("simulator", None),
            target => return target.to_string(),
        };
        let target = match name {
            Some(name) => format!("{kind} {name}"),
            None => kind.to_owned(),
        };
        if self.name == "default" || name == Some(&self.name) {
            target
        } else {
            format!("{target} \"{}\"", self.name)
        }
    }
}

/// An operation waiting for, or refused by, the user's decision on a request.
#[derive(Debug)]
pub struct Undecided(pub Box<ApprovalRequest>);

impl fmt::Display for Undecided {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "approval {} for {} is {}",
            self.0.number,
            self.0.subject(),
            self.0.status
        )
    }
}

impl std::error::Error for Undecided {}

/// The approval requests of `space`, or of every space the caller decides.
pub async fn list(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
) -> Result<Vec<ApprovalRequest>> {
    request(daemon, ApprovalMethod::ListApprovals { space }).await
}

/// The request `number`.
pub async fn show(daemon: &impl Daemon, number: ItemNumber) -> Result<Box<ApprovalRequest>> {
    request(daemon, ApprovalMethod::ShowApproval { number }).await
}

/// Approve or deny the request `number`, which `shown` names when the user
/// chose it from a list, so a request reusing its number is refused. Only
/// proposals take `details`; an approval runs with `env`.
pub async fn decide(
    daemon: &impl Daemon,
    number: ItemNumber,
    approve: bool,
    shown: Option<Box<ApprovalRequest>>,
    details: ProposalDecision,
    env: BTreeMap<String, String>,
) -> Result<Box<ApprovalRequest>> {
    let method = ApprovalMethod::DecideApproval {
        number,
        approve,
        shown,
        details,
        env: Some(env),
    };
    request(daemon, method).await
}

/// Pass the request `number`, routed to `space`, up to the space above it
/// or to the user.
pub async fn escalate(
    daemon: &impl Daemon,
    space: SpaceSelector,
    number: ItemNumber,
    shown: Option<Box<ApprovalRequest>>,
) -> Result<Box<ApprovalRequest>> {
    let method = ApprovalMethod::EscalateApproval {
        space,
        number,
        shown,
    };
    request(daemon, method).await
}

/// Withdraw `space`'s pending request `number`.
pub async fn withdraw(
    daemon: &impl Daemon,
    space: SpaceSelector,
    number: ItemNumber,
) -> Result<Box<ApprovalRequest>> {
    request(daemon, ApprovalMethod::WithdrawApproval { space, number }).await
}

/// Whether the `push` permission holds back the push of `workspace`'s branch.
pub async fn push_held(daemon: &impl Daemon, workspace: SpaceSelector) -> Result<bool> {
    let BranchPush { held } = request(daemon, ApprovalMethod::BranchPush { workspace }).await?;
    Ok(held)
}

/// Whether `workspace` may make the push `push` itself, or the approval
/// request recorded for it instead.
pub async fn request_push(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    push: PushSpecification,
) -> Result<PushOutcome> {
    request(daemon, ApprovalMethod::RequestPush { workspace, push }).await
}

/// Decide the approved push `key` by how it ended, with `failure` when it
/// failed.
pub async fn finish_push(
    daemon: &impl Daemon,
    key: String,
    failure: Option<String>,
) -> Result<Box<ApprovalRequest>> {
    request(daemon, ApprovalMethod::FinishPush { key, failure }).await
}
