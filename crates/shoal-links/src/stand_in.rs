//! A stand-in for the rest of the daemon, which the links' tests build
//! their operations over.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, Transaction};
use shoal_core::config::{Config, Effective, repo::ConfigLayers};
use shoal_daemon_base::{
    alerts::{About, Alert, AlertKind, NewAlert, Sent, Unread},
    callers::Caller,
    config_state::ConfigState,
    file_access::FileAccess,
    forge_access::ForgeAccess,
    gates::Gates,
    item_numbers::ItemNumber,
    notifications::{Coordinators, Notifications, Source},
    operations::Operations,
    permitted_actions::Action,
};
use shoal_forge_types::{
    items::{
        link::{ItemKind, Selection},
        reference::ItemRef,
    },
    numbers::PullNumber,
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    agent::Agent,
    approvals::ApprovalRequest,
    config::permissions::{self, PermissionKey},
    model::{
        ChangedOnBothSides, ConflictCheck, EventCause, Repository, Selected, Space, SpaceRef,
        Workspace,
    },
    paths::Paths,
    processes::Leftover,
    protocol::operations::OperationSpec,
    space_names::SpaceSelector,
};
use shoal_refs::{BranchName, CommitId};
use shoal_store::Store;
use tokio::sync::watch;

use crate::{
    Links,
    daemon::{
        Approvals, Elsewhere, Messages, Permissions, ProposalKind, Records, Reviews, SpaceAgent,
        Spaces, Watches,
    },
    space::LinkSpace,
};

/// The links' service over `store` and the global config `text`, its
/// operations' files under `root`, with records that take every link
/// change.
pub fn service(root: &std::path::Path, store: Store, text: &str) -> Arc<Links> {
    let gates = Arc::new(Gates::default());
    let config = Arc::new(ConfigState::new(shoal_core::test_support::global_config(
        text,
    )));
    let forges = ForgeAccess::new(store.clone(), gates.clone(), config.clone());
    Arc::new(Links::new(
        store.clone(),
        gates.clone(),
        config,
        Arc::new(Notifications::new(store)),
        Arc::new(FileAccess::default()),
        Arc::new(forges),
        Arc::new(Operations::new(Paths::for_test(root), gates)),
        Arc::new(StandInRecords),
    ))
}

/// Record `workspace` and its repository in `store`.
pub async fn record(store: &Store, workspace: &Workspace) {
    let path = workspace.path.to_str().unwrap().to_owned();
    let (id, repository) = (workspace.id.clone(), workspace.repository_id.clone());
    let (name, branch) = (workspace.name.clone(), workspace.branch().cloned());
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES (?1,?2,?2,1)",
                rusqlite::params![repository, path],
            )?;
            tx.execute(
                "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace',?2,?3)",
                rusqlite::params![id, repository, name],
            )?;
            tx.execute(
                "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                    VALUES (?1,?2,?3,'ready','abc','/admin')",
                rusqlite::params![id, path, branch],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The repository `id` registered at its checkout `path`.
pub fn registered(id: &str, path: &std::path::Path) -> Repository {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "path": path,
        "source": path,
        "last_used": 0,
        "workspaces_dir": path.parent().unwrap(),
    }))
    .unwrap()
}

/// The daemon as the links' tests see it.
#[derive(Default)]
pub struct StandIn {
    /// Each space's assigned issues and PRs, in the order assigned.
    pub assigned: HashMap<SpaceId, Vec<ItemRef>>,
    /// The workspaces requests come from.
    pub workspaces: Vec<Workspace>,
    pub repositories: Vec<Repository>,
    /// The config every space's settings resolve under.
    pub config: Option<Config>,
    /// The config file each repository tracks, which its settings take over
    /// `config`.
    pub repository_files: HashMap<RepositoryId, String>,
    /// The spaces whose agent runs.
    pub agents: Mutex<HashSet<SpaceId>>,
}

impl StandIn {
    /// The workspace `id`, if the test gave one.
    fn find(&self, id: &SpaceId) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| workspace.id == *id)
    }

    /// The workspace `selector` names by its ID.
    fn selected(&self, selector: &SpaceSelector) -> Result<Workspace> {
        let id = selector.given_id().context(NOT_HERE)?;
        self.find(id).cloned().context(NOT_HERE)
    }

    /// The settings under the config the test gave.
    fn given_settings(&self) -> Result<Effective> {
        (self.config.as_ref().context(NOT_HERE)?).resolve(&ConfigLayers::default())
    }

    /// The settings of the repository `id`, under its tracked file where
    /// the test gave one.
    fn settings_of(&self, id: &RepositoryId) -> Result<Effective> {
        let Some(file) = self.repository_files.get(id) else {
            return self.given_settings();
        };
        let layers = ConfigLayers {
            worktree_file: shoal_core::config::repo::parse(file)?,
            ..ConfigLayers::default()
        };
        (self.config.as_ref().context(NOT_HERE)?).resolve(&layers)
    }
}

/// What a test reached that the stand-in does not offer.
const NOT_HERE: &str = "the links' stand-in has no repositories, spaces or forges";

impl Spaces for StandIn {
    fn repository<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Repository>> {
        let found = (self.repositories.iter()).find(|repository| repository.id == *id);
        let found = found.cloned().context(NOT_HERE);
        Box::pin(async { found })
    }

    fn space<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Space>> {
        let space = self.selected(selector).map(Space::from);
        Box::pin(async { space })
    }

    fn find_space<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Space>>> {
        let space = self.find(id).cloned().map(Space::from);
        Box::pin(async { Ok(space) })
    }

    fn verify_space<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn linked_items<'a>(
        &'a self,
        id: &'a SpaceId,
        kind: ItemKind,
    ) -> BoxFuture<'a, Result<Vec<ItemRef>>> {
        let items = (self.assigned.get(id).into_iter().flatten())
            .filter(|item| item.kind() == kind.into())
            .cloned()
            .collect();
        Box::pin(async { Ok(items) })
    }

    fn select_items<'a>(
        &'a self,
        _: &'a LinkSpace,
        _: &'a Selection,
        _: bool,
    ) -> BoxFuture<'a, Result<Selected>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn acting_settings<'a>(&'a self, _: &'a LinkSpace) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.given_settings();
        Box::pin(async { settings })
    }

    fn space_settings<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.given_settings();
        Box::pin(async { settings })
    }

    fn repository_settings<'a>(&'a self, id: &'a RepositoryId) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.settings_of(id);
        Box::pin(async { settings })
    }

    fn settings_in<'a>(&'a self, id: Option<&'a RepositoryId>) -> BoxFuture<'a, Result<Effective>> {
        let settings = match id {
            Some(id) => self.settings_of(id),
            None => self.given_settings(),
        };
        Box::pin(async { settings })
    }

    fn workspace_settings<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<Effective>> {
        let settings = self.given_settings();
        Box::pin(async { settings })
    }

    fn space_agent<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<SpaceAgent>> {
        let present = self.agents.lock().unwrap().contains(id);
        Box::pin(async move {
            Ok(SpaceAgent {
                done: false,
                present,
            })
        })
    }

    fn wake_cleanup(&self) {}

    fn registered_named<'a>(
        &'a self,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Repository>>> {
        Box::pin(async { Ok(None) })
    }

    fn tracked_agents(&self) -> BoxFuture<'_, Vec<SpaceId>> {
        let agents = self.agents.lock().unwrap().iter().cloned().collect();
        Box::pin(async { agents })
    }

    fn repository_workspaces<'a>(
        &'a self,
        _: &'a RepositoryId,
    ) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn reached_workspaces<'a>(&'a self, _: &'a Caller) -> BoxFuture<'a, Result<Vec<Workspace>>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn reaches<'a>(&'a self, _: &'a Caller, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn workspace<'a>(&'a self, selector: &'a SpaceSelector) -> BoxFuture<'a, Result<Workspace>> {
        let workspace = self.selected(selector);
        Box::pin(async { workspace })
    }

    fn workspace_by_id<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Workspace>> {
        let workspace = self.find(id).cloned().context(NOT_HERE);
        Box::pin(async { workspace })
    }

    fn find_workspace<'a>(&'a self, id: &'a SpaceId) -> BoxFuture<'a, Result<Option<Workspace>>> {
        let workspace = self.find(id).cloned();
        Box::pin(async { Ok(workspace) })
    }

    fn verify_worktree<'a>(&'a self, _: &'a Workspace) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn repositories(&self) -> BoxFuture<'_, Result<Vec<Repository>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn branch_owner<'a>(
        &'a self,
        _: &'a RepositoryId,
        _: &'a BranchName,
    ) -> BoxFuture<'a, Result<Option<SpaceId>>> {
        Box::pin(async { Ok(None) })
    }

    fn pull_workspace<'a>(
        &'a self,
        _: &'a RepositoryId,
        _: &'a ItemRef,
        _: PullNumber,
    ) -> BoxFuture<'a, Result<Option<Workspace>>> {
        Box::pin(async { Ok(None) })
    }

    fn proposing_agent<'a>(&'a self, _: &'a Caller) -> BoxFuture<'a, Option<Agent>> {
        Box::pin(async { None })
    }

    fn present_spaces(&self) -> BoxFuture<'_, Result<Vec<Space>>> {
        let spaces = self.workspaces.iter().cloned().map(Space::from).collect();
        Box::pin(async { Ok(spaces) })
    }

    fn watched_conflicts<'a>(
        &'a self,
        _: &'a SpaceSelector,
        _: bool,
    ) -> BoxFuture<'a, Result<Option<ConflictCheck>>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Watches for StandIn {
    fn item_updates_queued(&self) {}

    fn watch_item_updates(&self) -> watch::Receiver<u64> {
        watch::channel(0).1
    }

    fn agent_changed(&self) {}

    fn item_merged(&self, _: &RepositoryId) {}

    fn draft_changed(&self, _: &SpaceId) {}
}

impl Messages for StandIn {
    fn unread_messages<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<Unread>> {
        Box::pin(async { Ok(Unread::default()) })
    }

    fn deliverable_messages<'a>(&'a self, _: &'a Space) -> BoxFuture<'a, Result<Vec<Alert>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn settled_messages<'a>(
        &'a self,
        _: &'a Space,
        messages: Vec<Alert>,
    ) -> BoxFuture<'a, Result<Vec<Alert>>> {
        Box::pin(async { Ok(messages) })
    }

    fn undelivered_leftovers(&self, _: &SpaceId, _: usize) -> Result<Vec<Leftover>> {
        Ok(Vec::new())
    }

    fn wakes_held<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }

    fn watch_messages(&self) -> watch::Receiver<u64> {
        watch::channel(0).1
    }

    fn message_queued(&self) {}
}

impl Elsewhere for StandIn {
    fn reaches_hub(&self) -> bool {
        false
    }

    fn coordinator_elsewhere<'a>(&'a self, _: &'a SpaceId) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn pass_up_activity<'a>(
        &'a self,
        _: SpaceId,
        _: &'a SpaceRef,
        _: String,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Approvals for StandIn {
    fn announce<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn ensure_gate_hears<'a>(&'a self, _: &'a ApprovalRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn tell_space<'a>(&'a self, _: &'a SpaceId, _: AlertKind, _: String) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn notify_about(&self, _: About, _: AlertKind, _: String) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn start_operation(
        &self,
        _: OperationSpec,
        _: Option<BTreeMap<String, String>>,
    ) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { bail!(NOT_HERE) })
    }
}

impl Reviews for StandIn {
    fn merge_review_required(&self, _: &Effective, _: Option<&Caller>) -> bool {
        false
    }

    fn require_merge_review<'a>(
        &'a self,
        _: &'a Workspace,
        _: &'a CommitId,
        _: &'a ItemRef,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn pr_review_required(&self, _: &Effective, _: Option<&Caller>) -> bool {
        false
    }

    fn require_pr_review<'a>(
        &'a self,
        _: &'a Workspace,
        _: &'a CommitId,
        _: Option<&'a Caller>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Coordinators for StandIn {
    fn tell<'a>(&'a self, _: NewAlert, _: bool, _: Source) -> BoxFuture<'a, Option<Sent>> {
        Box::pin(async { None })
    }
}

impl Permissions for StandIn {
    fn authorize_action<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: Action,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn caller_permissions<'a>(
        &'a self,
        _: &'a Caller,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<permissions::Permissions>> {
        Box::pin(async { bail!(NOT_HERE) })
    }

    fn authorize_actions<'a>(
        &'a self,
        _: Option<&'a Caller>,
        _: Vec<Action>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn refuse_denied<'a>(
        &'a self,
        _: &'a Caller,
        _: PermissionKey,
        _: Option<&'a RepositoryId>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

/// Records as the links' tests see them: every space takes link changes,
/// none is below another, and no approval request is recorded.
#[derive(Default)]
pub struct StandInRecords;

impl Records for StandInRecords {
    fn ensure_accepts_links(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn accepts_links(&self, _: &Connection, _: &SpaceId) -> Result<bool> {
        Ok(true)
    }

    fn forget_ready_marks(&self, _: &Connection, _: &SpaceId, _: &[String]) -> Result<()> {
        Ok(())
    }

    fn require_ready_space(&self, _: &Connection, _: &SpaceId) -> Result<()> {
        Ok(())
    }

    fn delegated(&self, _: &Connection, _: &SpaceId, _: &[SpaceId]) -> Result<Vec<SpaceRef>> {
        Ok(Vec::new())
    }

    fn receiving_paths(
        &self,
        _: &Connection,
        id: &SpaceId,
        _: &[SpaceId],
    ) -> Result<Vec<Vec<SpaceId>>> {
        Ok(vec![vec![id.clone()]])
    }

    fn delegated_elsewhere(
        &self,
        _: &Connection,
        _: &[SpaceId],
    ) -> Result<Vec<(SpaceId, Vec<SpaceRef>)>> {
        Ok(Vec::new())
    }

    fn record_proposal(
        &self,
        _: &Transaction<'_>,
        _: ApprovalRequest,
        _: ProposalKind,
    ) -> Result<(ApprovalRequest, bool)> {
        bail!(NOT_HERE)
    }

    fn save_proposal(&self, _: &Transaction<'_>, _: &ApprovalRequest) -> Result<()> {
        bail!(NOT_HERE)
    }

    fn forget_proposal(&self, _: &Transaction<'_>, _: &ApprovalRequest) -> Result<()> {
        bail!(NOT_HERE)
    }

    fn retire_approval(&self, _: &Transaction<'_>, _: &ApprovalRequest) -> Result<()> {
        bail!(NOT_HERE)
    }

    fn approval_by_number(&self, _: &Connection, _: ItemNumber) -> Result<Option<ApprovalRequest>> {
        Ok(None)
    }

    fn approval_by_key(&self, _: &Connection, _: &str) -> Result<Option<ApprovalRequest>> {
        Ok(None)
    }

    fn approval_requests(
        &self,
        _: &Connection,
        _: Option<&SpaceId>,
    ) -> Result<Vec<ApprovalRequest>> {
        Ok(Vec::new())
    }

    fn changed_on_both_sides(&self, _: &Connection) -> Result<Vec<(SpaceId, ChangedOnBothSides)>> {
        Ok(Vec::new())
    }

    fn builds_on(&self, _: &Connection, base: &SpaceId, id: &SpaceId) -> Result<bool> {
        Ok(base == id)
    }

    fn change_base(
        &self,
        _: &Transaction<'_>,
        _: &SpaceId,
        _: &SpaceId,
        _: Option<&SpaceId>,
        _: EventCause,
    ) -> Result<bool> {
        bail!(NOT_HERE)
    }

    fn queue_shoal_message(
        &self,
        _: &Transaction<'_>,
        _: &SpaceId,
        _: AlertKind,
        _: &str,
    ) -> Result<()> {
        bail!(NOT_HERE)
    }

    fn checked_branch(&self, _: &Connection, _: &SpaceId) -> Result<Option<String>> {
        Ok(None)
    }
}
