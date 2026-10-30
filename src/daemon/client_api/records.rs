//! The daemon's records as the client API's.
use shoal_client::records as api;

use crate::{
    daemon::{approvals::ApprovalRequest, questions::Question},
    forge::items::link::ItemKind,
    model,
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
    match kind {
        ItemKind::Issue => api::ItemKind::Issue,
        ItemKind::Pr => api::ItemKind::Pr,
    }
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
        workspace_id: request.workspace_id,
        workspace: request.workspace,
        target: request.target.to_string(),
        name: request.name,
        reason: request.reason,
        status: api::ApprovalStatus::from(request.status.to_string().as_str()),
        created_at: request.created_at,
    }
}
