//! Linking and unlinking a workspace's issues and PRs.
use shoal_client::{
    AddLink, ApiError, RemoveLink,
    records::{ItemKind, Relation},
};

use super::super::{Session, errors};
use crate::{forge::items::link, model, protocol::ItemMethod};

pub(super) async fn link(session: &Session, params: AddLink) -> Result<(), ApiError> {
    session
        .ask(ItemMethod::LinkItem {
            workspace: params.workspace,
            kind: item_kind(&params.kind)?,
            url: params.item,
            relation: relation(&params.relation)?,
            title: None,
        })
        .await
}

pub(super) async fn unlink(session: &Session, params: RemoveLink) -> Result<(), ApiError> {
    session
        .ask(ItemMethod::UnlinkItems {
            workspace: params.workspace,
            relation: relation(&params.relation)?,
            kind: Some(item_kind(&params.kind)?),
            url: Some(params.item),
        })
        .await
}

fn item_kind(kind: &ItemKind) -> Result<link::LinkKind, ApiError> {
    match kind {
        ItemKind::Issue => Ok(link::LinkKind::Issue),
        ItemKind::Pr => Ok(link::LinkKind::Pr),
        ItemKind::Ci => Ok(link::LinkKind::Ci),
        ItemKind::Unknown(kind) => Err(errors::invalid(format!("unknown item kind {kind}"))),
    }
}

fn relation(relation: &Relation) -> Result<model::Relation, ApiError> {
    match relation {
        Relation::Assigned => Ok(model::Relation::Assigned),
        Relation::Opened => Ok(model::Relation::Opened),
        Relation::Related => Ok(model::Relation::Related),
        Relation::Unknown(relation) => Err(errors::invalid(format!("unknown relation {relation}"))),
    }
}
