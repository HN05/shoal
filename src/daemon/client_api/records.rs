//! The daemon's records as the client API's.
use std::collections::HashMap;

use shoal_client::records::{self as api, MessageRecipient};

use crate::{
    daemon::{
        agent_messages::AgentMessage, approvals::ApprovalRequest, notifications::Notification,
        questions::Question,
    },
    forge::{
        items::link::{ItemKind, LinkKind},
        updates::Snapshot,
    },
    model::{self, Relation},
};

use super::agents::Agents;

pub(super) fn repository(repository: model::Repository) -> api::Repository {
    api::Repository {
        id: repository.id,
        name: repository.name,
        path: repository.path,
        source: repository.source,
    }
}

pub(super) fn workspace(workspace: &model::Workspace, agents: &Agents) -> api::Workspace {
    api::Workspace {
        id: workspace.id.clone(),
        repository_id: workspace.repository_id.clone(),
        name: workspace.name.clone(),
        title: workspace.title.clone(),
        path: workspace.path.clone(),
        branch: workspace.branch.clone(),
        state: api::WorkspaceState::from(workspace.state.to_string().as_str()),
        error: workspace.error.clone(),
        done: workspace.done,
        base_workspace: workspace.base_workspace.clone().map(workspace_ref),
        parent_workspace: workspace.parent_workspace.clone().map(workspace_ref),
        holds: workspace.holds.iter().cloned().map(hold).collect(),
        ready_marks: workspace
            .review
            .iter()
            .map(|mark| api::ReadyMark {
                item: mark
                    .kind
                    .zip(mark.url.clone())
                    .map(|(kind, url)| api::Item {
                        kind: item_kind(kind),
                        url,
                    }),
                head: mark.head.clone(),
                created_at: mark.created_at,
            })
            .collect(),
        agent: agents.status(workspace),
    }
}

fn workspace_ref(workspace: model::WorkspaceRef) -> api::WorkspaceRef {
    api::WorkspaceRef {
        id: workspace.id,
        name: workspace.name,
        branch: workspace.branch,
    }
}

pub(super) fn hold(hold: model::WorkspaceHold) -> api::Hold {
    api::Hold {
        name: hold.name,
        reason: hold.reason,
        created_at: hold.created_at,
    }
}

fn item_kind(kind: ItemKind) -> api::ItemKind {
    link_kind(kind.into())
}

fn link_kind(kind: LinkKind) -> api::ItemKind {
    match kind {
        LinkKind::Issue => api::ItemKind::Issue,
        LinkKind::Pr => api::ItemKind::Pr,
        LinkKind::Ci => api::ItemKind::Ci,
    }
}

/// Each issue and PR `workspace` links, with what the daemon last observed
/// of it on its forge, from `activity` by URL.
pub(super) fn links(
    workspace: &model::Workspace,
    activity: &HashMap<String, Snapshot>,
) -> Vec<api::Link> {
    let links = &workspace.links;
    let assigned = links
        .issue
        .iter()
        .map(|url| (LinkKind::Issue, url, links.issue_title.clone()))
        .chain(links.prs.iter().map(|url| (LinkKind::Pr, url, None)))
        .map(|(kind, url, title)| (Relation::Assigned, kind, url, title));
    let recorded = [
        (Relation::Opened, &links.opened),
        (Relation::Related, &links.related),
    ]
    .into_iter()
    .flat_map(|(relation, items)| {
        items
            .iter()
            .map(move |item| (relation, item.kind, &item.url, item.title.clone()))
    });
    assigned
        .chain(recorded)
        .map(|(relation, kind, url, title)| {
            let observed = activity.get(url);
            api::Link {
                id: format!("{}/{relation}/{url}", workspace.id),
                workspace_id: workspace.id.clone(),
                kind: link_kind(kind),
                url: url.clone(),
                relation: api::Relation::from(relation.to_string().as_str()),
                title,
                state: observed
                    .map(Snapshot::state)
                    .filter(|state| !state.is_empty())
                    .map(str::to_owned),
                head: observed
                    .map(Snapshot::head)
                    .filter(|head| !head.is_empty())
                    .map(str::to_owned),
                checks: observed.map_or_else(Vec::new, |observed| {
                    observed
                        .checks()
                        .map(|(name, state, complete)| api::Check {
                            name: name.into(),
                            state: state.into(),
                            complete,
                        })
                        .collect()
                }),
                conflict: observed.and_then(Snapshot::conflict),
            }
        })
        .collect()
}

pub(super) fn question(question: Question) -> api::Question {
    api::Question {
        id: question.id,
        workspace_id: question.workspace_id,
        workspace: question.workspace,
        question: question.ask.question,
        recommendation: question.ask.recommendation,
        reason: question.ask.reason,
        choices: question.ask.choices,
        created_at: question.created_at,
    }
}

pub(super) fn approval(request: ApprovalRequest) -> api::Approval {
    api::Approval {
        id: request.id,
        key: request.key,
        workspace_id: request.workspace_id,
        workspace: request.workspace,
        target: request.target.to_string(),
        name: request.name,
        reason: request.reason,
        status: api::ApprovalStatus::from(request.status.to_string().as_str()),
        created_at: request.created_at,
    }
}

/// A message queued for the agent of the workspace `workspace` names.
pub(super) fn agent_message(message: AgentMessage, workspace: &model::Workspace) -> api::Message {
    api::Message {
        id: format!("agent/{}", message.id),
        recipient: MessageRecipient::Agent,
        workspace_id: Some(workspace.id.clone()),
        workspace: Some(workspace.name.clone()),
        text: message.message,
        attachments: message.attachments,
        created_at: message.created_at,
    }
}

/// A workspace process's message to the user, which its notification records.
pub(super) fn user_message(notification: &Notification) -> api::Message {
    api::Message {
        id: format!("user/{}", notification.id),
        recipient: MessageRecipient::User,
        workspace_id: None,
        workspace: notification.workspace.clone(),
        text: notification.message.clone(),
        attachments: Vec::new(),
        created_at: notification.created_at,
    }
}

pub(super) fn notification(notification: Notification) -> api::Notification {
    api::Notification {
        id: notification.id,
        kind: notification.kind.to_string(),
        workspace: notification.workspace,
        message: notification.message,
        read: notification.read,
        created_at: notification.created_at,
    }
}
