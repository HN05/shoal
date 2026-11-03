//! Approval requests: what a workspace's agent asks the user, or a coordinating
//! agent, to allow.
use std::{fmt, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    client::{
        resources::{Definition, LockMode, RepositoryView, Scope},
        sim::Profile,
    },
    config::repo::ConflictPolicy,
    forge::items::{
        action::Edit,
        attach::Attachment,
        create::{NewIssue, NewPull},
    },
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
/// `secret/<name>`, `hold/<name>`, `merge`, `issue_open`, `pr_open` or `push`.
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

/// The name of a workspace's single merge request.
pub const MERGE: &str = "branch";

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resource { scope, pool } => write!(f, "resource/{scope}/{pool}"),
            Self::Port(name) => write!(f, "port/{name}"),
            Self::Simulator => f.write_str("simulator"),
            Self::Secret(name) => write!(f, "secret/{name}"),
            Self::Merge => f.write_str("merge"),
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
/// default branch afterwards, and the sensitive files it warned about; later
/// commits or another warning need a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeSpecification {
    pub branch: String,
    pub head: String,
    /// Where the merge pushes the default branch afterwards: the remote and
    /// the ref there, so another destination needs another decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<crate::model::MergePush>,
    /// The sensitive files the merge brings to a default branch that changed
    /// them too, so a warning recorded after the decision needs another.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_on_both_sides: Vec<String>,
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
    /// The number the user decides it by; reused once the request is deleted.
    #[serde(default)]
    pub id: i64,
    /// Stable identity behind the user-facing number, carried through displayed snapshots.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    pub workspace_id: String,
    #[serde(default)]
    pub workspace: String,
    /// The repository that owns a proposed issue, which outlives the
    /// workspace that proposed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
    pub target: Target,
    pub name: String,
    pub specification: Specification,
    pub lifetime: Lifetime,
    pub reason: String,
    pub status: DecisionStatus,
    pub created_at: u64,
    pub decided_at: Option<u64>,
    /// False for workspace grants retained after release.
    pub active: bool,
    /// Whether the request went up the workspace tree to find its decider.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub routed: bool,
    /// The workspace whose coordinating agent decides the request, since it
    /// holds a grant for the same thing; the user decides without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decider: Option<String>,
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
    /// Whether approval also assigns the issue to the workspace.
    pub link: bool,
    /// The proposing workspace's branch; `None` for one at a commit.
    pub branch: Option<String>,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
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
    pub agent: Option<String>,
    /// What is left to do to the PR once approval posted it, such as its
    /// labels and reviewers, until it is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<Edit>,
    /// The push of the branch that approval runs before opening the PR,
    /// when `[permissions] push` held it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<PushSpecification>,
}

/// A push of one ref that `[permissions] push` held back, which approval
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
    pub agent: Option<String>,
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
    Approval(i64),
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
