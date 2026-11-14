//! What the gate is told of a request: the settings and rules of its
//! repository's checkout, the requesting space and its issues and PRs, the
//! commits the request brings and what Shoal finds missing in it.
use anyhow::{Context, Result};
use shoal_refs::CommitId;

use super::{
    prompt,
    subject::{self, Subject},
};
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    config::{Effective, gate::GateRules},
    daemon::{
        approvals::{ApprovalRequest, Specification, governing},
        log,
        workspace::Manager,
    },
    forge::{
        conventions,
        items::link::{ItemKind, Selection},
    },
    model::{PresentSpace, Repository},
    protocol::ItemContext,
};

/// The configuration gives the gate no agent it can run, which only the
/// user fixes.
#[derive(Debug)]
pub(super) struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the gate's configuration names no agent it can run")
    }
}

/// The most commits the gate is shown of one request.
const COMMIT_LIMIT: usize = 50;

/// Everything the gate's run of one request needs.
pub(super) struct Case {
    /// The registered checkout it runs in, whose config it reads.
    pub repository: Repository,
    pub settings: Effective,
    pub agent: Agent,
    pub fallback: Option<Agent>,
    pub rules: GateRules,
    /// What a session must have opened with to be resumed: the agent's
    /// name and the rules, as JSON.
    pub opened_with: String,
    pub subject: Subject,
    pub action: String,
    /// The opening a new session of the space starts with.
    pub opening: String,
    /// The space's linked issues and PRs as they are now, which a resumed
    /// session is given again.
    pub items: Vec<String>,
    /// The requesting workspace, where the request's commits are read.
    pub worktree: Option<std::path::PathBuf>,
    /// The commit the request binds, and the one its change builds on.
    pub head: Option<CommitId>,
    pub base: Option<CommitId>,
    pub warnings: Vec<String>,
}

impl Manager {
    /// The case of `request` for the gate.
    pub(super) async fn gate_case(&self, request: &ApprovalRequest) -> Result<Case> {
        let space = self.space_by_id(&request.space_id).await?;
        let repository_id = (request.repository_id.as_ref())
            .or(space.repository_id())
            .context("the request names no registered repository")?
            .clone();
        let repository = self.repository(&repository_id).await?;
        // The checkout's config, which the requesting agent cannot write.
        let settings = (self.repository_settings(&repository_id).await)
            .map_err(|error| error.context(Unavailable))?;
        let (agent, fallback) =
            gate_agents(&settings).map_err(|error| error.context(Unavailable))?;
        let rules = settings.gate.rules(&governing(request));
        let opened_with = serde_json::json!([agent.name(), rules.instructions, rules.escalate]);
        let workspace = space.worktree();
        let branch = workspace.and_then(|workspace| workspace.branch.as_ref());
        let subject = Subject::of(request, branch);
        let items = self.gate_items(&space).await;
        let opening = prompt::opening(
            &rules,
            &prompt::Space {
                name: space.name().as_str(),
                kind: &space.label(),
                mode: space.mode().map(|mode| mode.name.as_str()),
                branch: branch.map(|branch| branch.as_str()),
                items: &items,
            },
        );
        let base = match workspace {
            Some(workspace) => (self.diff_base(&SpaceSelector::id(&workspace.id)).await)
                .map(|base| base.commit)
                .ok(),
            None => None,
        };
        let warnings = self.gate_warnings(request, &repository, &settings).await;
        Ok(Case {
            action: subject::action(request),
            worktree: workspace.map(|workspace| workspace.path.clone()),
            head: bound_commit(request),
            repository,
            settings,
            agent,
            fallback,
            opened_with: opened_with.to_string(),
            rules,
            subject,
            opening,
            items,
            base,
            warnings,
        })
    }

    /// The space's linked issues and PRs as the opening lists them; a failed
    /// lookup leaves them out.
    async fn gate_items(&self, space: &PresentSpace) -> Vec<String> {
        let selector = SpaceSelector::id(space.id());
        let context = match space {
            PresentSpace::Workspace(_) => ItemContext::Workspace(selector),
            PresentSpace::Chat(_) => ItemContext::Chat(selector),
        };
        let selection = Selection {
            kind: None,
            input: None,
        };
        let viewed = match self.view_items(&context, &selection, false, None).await {
            Ok(viewed) => viewed,
            Err(error) => {
                log!("the gate's view of {}: {error:#}", space.name());
                return Vec::new();
            }
        };
        let relation = |url: &crate::forge::items::reference::ItemRef| {
            let item = viewed.items.iter().find(|item| item.url == *url);
            let relations = item.map(|item| &item.relations).into_iter().flatten();
            relations
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        (viewed.views.iter())
            .filter_map(|view| {
                let details = view.details.as_ref()?;
                let url = view.url.to_string();
                Some(prompt::item(&relation(&view.url), details, &url))
            })
            .collect()
    }

    /// What an issue or PR proposal leaves out of its repository's
    /// conventions; none for another request, an issue in another
    /// repository, or a forge that cannot be read.
    async fn gate_warnings(
        &self,
        request: &ApprovalRequest,
        repository: &Repository,
        settings: &Effective,
    ) -> Vec<String> {
        let (kind, labels, body, required) = match &request.specification {
            Specification::Pull(pull) => (
                ItemKind::Pr,
                &pull.pull.labels,
                &pull.pull.body,
                &settings.pr.required_labels,
            ),
            Specification::Issue(issue) if issue.repository.is_none() => (
                ItemKind::Issue,
                &issue.issue.labels,
                &issue.issue.body,
                &settings.issue.required_labels,
            ),
            _ => return Vec::new(),
        };
        match self.checkout_forge(repository).await {
            Ok(forge) => {
                let path = &repository.path;
                conventions::proposal_warnings(&forge, path, kind, labels, body, required)
            }
            Err(error) => {
                log!("the gate's warnings for {}: {error:#}", request.number);
                Vec::new()
            }
        }
    }

    /// The request's commits for the gate: those neither its change's base
    /// nor `seen`, the commit the session's last verdict saw, has, so a
    /// rebase onto newer base commits brings none of them.
    pub(super) async fn gate_commits(&self, case: &Case, seen: Option<&str>) -> Option<String> {
        let (worktree, head) = (case.worktree.as_ref()?, case.head.as_ref()?);
        let base = case.base.as_ref().map(CommitId::as_str);
        let excluded: Vec<&str> = seen.into_iter().chain(base).collect();
        let log = crate::git::log_stat(worktree, head.as_str(), &excluded, COMMIT_LIMIT).await;
        match log {
            Ok(log) => Some(log),
            Err(error) => {
                log!("the gate's commits for {head}: {error:#}");
                None
            }
        }
    }
}

/// The agent that decides, `[gate] agent` else `agent.default`, and the
/// one that decides once more after it fails.
fn gate_agents(settings: &Effective) -> Result<(Agent, Option<Agent>)> {
    let agent = (settings.gate.agent.clone())
        .or_else(|| settings.agent.default.clone())
        .context("no gate agent: set gate.agent or agent.default")?;
    settings.check_reviewer(&agent)?;
    let fallback = (settings.gate.fallback_agent.clone())
        .filter(|fallback| *fallback != agent && settings.check_reviewer(fallback).is_ok());
    Ok((agent, fallback))
}

/// The commit `request` binds: what it pushes, opens a PR at, merges or
/// unmerges.
fn bound_commit(request: &ApprovalRequest) -> Option<CommitId> {
    match &request.specification {
        Specification::Push(push) => push.commit.clone(),
        Specification::Pull(pull) => Some(pull.commit.clone()),
        Specification::Merge(merge) => Some(merge.head.clone()),
        Specification::Unmerge(unmerge) => Some(unmerge.source.clone()),
        _ => None,
    }
}
