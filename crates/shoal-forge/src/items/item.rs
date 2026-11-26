//! Reading an issue or PR as one REST read reports it, in the same shape for
//! every forge.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    items::Items,
    transport::{
        account::Account,
        api::{Request, TransportRequests},
    },
};
use shoal_forge_types::{
    ForgeRepo,
    items::{item::Current, link::ItemKind},
};

/// Reading issues and PRs.
#[shoal_forge_derive::extension(pub ItemRequests, pub(crate) ItemSteps)]
impl ForgeRepo {
    /// The item's REST endpoint.
    pub(crate) fn item_endpoint(&self, kind: ItemKind, number: u64) -> String {
        let resource = match kind {
            ItemKind::Issue => "issues",
            ItemKind::Pr => self.kind().pull_resource(),
        };
        self.repo_endpoint(&format!("{resource}/{number}"))
    }

    /// Read a REST item; an issue read that returns a PR is refused.
    pub(crate) fn current(&self, kind: ItemKind, number: u64, value: &Value) -> Result<Current> {
        ensure!(
            value[self.kind().number_field()] == number,
            "the forge returned a different item than #{number}"
        );
        self.kind().current(self, kind, number, value)
    }

    /// Read an issue or PR.
    pub async fn read(&self, account: &Account, kind: ItemKind, number: u64) -> Result<Current> {
        let value = self.read_value(account, kind, number).await?;
        self.current(kind, number, &value)
    }

    /// The item as the forge reports it, for the fields `Current` leaves out.
    pub(crate) async fn read_value(
        &self,
        account: &Account,
        kind: ItemKind,
        number: u64,
    ) -> Result<serde_json::Value> {
        self.send(account, &Request::get(self.item_endpoint(kind, number)))
            .await
    }

    /// Whether `number` names an issue or a PR. Only forges that number both
    /// in one sequence can tell; elsewhere the caller names the kind.
    pub async fn item_kind(&self, account: &Account, number: u64) -> Result<ItemKind> {
        ensure!(
            self.kind().numbers_items_together(),
            "this forge numbers issues and PRs separately; put issue or pr before #{number}"
        );
        let value = self.read_value(account, ItemKind::Issue, number).await?;
        issue_read_kind(number, &value)
    }
}

/// The kind of the item a GitHub or Forgejo issue read returned, which
/// reports a PR as an issue with a `pull_request` field.
pub(crate) fn issue_read_kind(number: u64, value: &Value) -> Result<ItemKind> {
    ensure!(
        value["number"] == number,
        "the forge returned a different item than #{number}"
    );
    Ok(if value["pull_request"].is_null() {
        ItemKind::Issue
    } else {
        ItemKind::Pr
    })
}

pub(crate) fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

/// The `field` of each user in `users`, a list a read may leave out.
pub(crate) fn user_names(users: &Value, field: &str) -> Vec<String> {
    users
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|user| text(&user[field]))
        .collect()
}

#[cfg(test)]
mod tests;
