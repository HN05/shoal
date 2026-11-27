//! Running here the agent of a recorded space's starting launch.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::{Starting, enter_workspace};
use crate::launch::{LaunchOptions, ModelRequest, Opening};
use crate::{
    agent::Agent,
    agents::HomeRecords,
    client::queue::Hold,
    config::{modes::ModeName, templates::Subject},
    hosting,
    protocol::{ItemContext, operations::QueuedLaunch},
    terminal::{Owner, PlaceName},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

/// Run here the agent of the starting launch `entry`, which this process
/// holds, in the space it names. The place it runs in is named as
/// `place_name` says. A failed start fails the entry.
pub async fn run_entry(
    starting: &impl Starting,
    entry: &QueueEntryId,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let _hold = Hold::start(starting.caller().paths, entry).await;
    let mut scope = SpaceScope { held: None };
    let started = start_entry(starting, entry, place_name, &mut scope).await;
    // The space's scope reaches its entry, which the starter's may no
    // longer, so the failure is reported before the scope is revoked.
    if let Err(error) = &started {
        crate::queued_starts::fail(starting.caller().paths, entry, error).await;
    }
    scope.leave(starting).await;
    started
}

/// Start the agent of the starting launch `entry` as [`run_entry`] does,
/// once the daemon opened the space for it. When this process is scoped, it
/// runs in the `scope` this process issues for the space.
async fn start_entry(
    starting: &impl Starting,
    entry: &QueueEntryId,
    place_name: Option<PlaceName>,
    scope: &mut SpaceScope,
) -> Result<i32> {
    let queued = crate::client::queue::open_start(starting.caller().paths, entry.clone()).await?;
    let start = queued.start;
    let space = (queued.entry.space()).context("the starting launch names no space")?;
    let space = crate::homes::records(starting.caller().paths, &SpaceSelector::id(space)).await?;
    host(starting, &space, place_name, Some(&start.launch.agent)).await?;
    *scope = SpaceScope::act_in(starting, &space).await?;
    launch_in(starting, space, entry, start.launch).await
}

/// Launch the agent of `launch` in `space`, taking the slot of `entry`.
async fn launch_in(
    starting: &impl Starting,
    space: HomeRecords,
    entry: &QueueEntryId,
    launch: QueuedLaunch,
) -> Result<i32> {
    let subject = subject(starting, &space, &launch).await?;
    crate::env::launch_queue_entry(entry);
    self::launch(starting, space, launch, subject).await
}

/// Host `space` in the place this process runs in, named as `place_name`
/// says, for `agent`, else for a shell, which enters a workspace once the
/// agent ends.
pub async fn host(
    starting: &impl Starting,
    space: &HomeRecords,
    place_name: Option<PlaceName>,
    agent: Option<&Agent>,
) -> Result<()> {
    let label = place_label(space, place_name);
    let runs_agent = agent.is_some();
    match space {
        HomeRecords::Workspace(workspace) => {
            let owner = Owner::Workspace(&workspace.id);
            hosting::host_space(starting.caller(), owner, runs_agent, &label).await?;
            enter_workspace(starting, workspace, agent).await
        }
        HomeRecords::Chat { chat, .. } => {
            hosting::host_space(starting.caller(), Owner::Chat(&chat.id), runs_agent, &label).await
        }
    }
}

/// The label of the place hosting `space`, named as `place_name` says: a
/// workspace's from its branch and name, a chat's its name.
fn place_label(space: &HomeRecords, place_name: Option<PlaceName>) -> String {
    match space {
        HomeRecords::Workspace(workspace) => {
            let (branch, name) = (workspace.branch_or_name(), workspace.name.as_str());
            place_name.map_or_else(|| name.to_owned(), |place| place.render(branch, name))
        }
        HomeRecords::Chat { chat, .. } => chat.name.to_string(),
    }
}

/// What the agent of `launch` in `space` starts on. That is the issue it
/// names, read as the space reads it, open or closed, since the work goes
/// on whatever the issue's state. Else it is the CI run it names as it
/// stands now, else its prompt.
async fn subject(
    starting: &impl Starting,
    space: &HomeRecords,
    launch: &QueuedLaunch,
) -> Result<Option<Subject>> {
    let context = ItemContext::space(SpaceSelector::id(space.home()?.id()));
    let paths = starting.caller().paths;
    if let Some(issue) = &launch.issue {
        let input = issue.url.clone().into();
        let issue = crate::client::forge_reads::linked_issue_details(paths, context, input);
        return Ok(Some(Subject::issue(&issue.await?)));
    }
    if let Some(run) = &launch.run {
        let (target, input) = (context.into(), run.clone().into());
        let view = crate::client::views::run_with_jobs(paths, &target, &input).await?;
        return Subject::run(run, &view).map(Some);
    }
    Ok(launch.prompt.clone().map(Subject::told))
}

/// Launch the agent of `launch` in `space`, starting on `subject`.
async fn launch(
    starting: &impl Starting,
    space: HomeRecords,
    launch: QueuedLaunch,
    subject: Option<Subject>,
) -> Result<i32> {
    let QueuedLaunch {
        agent,
        mode,
        narrowing,
        model,
        args,
        fork,
        ..
    } = launch;
    let opening = Opening::New {
        mode: mode.unwrap_or(ModeName::DEFAULT),
        subject,
        note: None,
    };
    let options = LaunchOptions {
        fork,
        model: ModelRequest::Override(model),
        narrowing,
        ..Default::default()
    };
    let args = args.into_iter().map(OsString::from).collect();
    crate::launch::launch_agent(starting.caller(), agent, space, opening, args, options).await
}

/// The scope a scoped process issues itself for the space whose agent it
/// runs, which that agent outlasts the scope it inherited with.
struct SpaceScope {
    held: Option<(SpaceSelector, ScopeToken)>,
}

impl SpaceScope {
    /// Make this process act in a scope it issues for `space`, when it is
    /// scoped.
    async fn act_in(starting: &impl Starting, space: &HomeRecords) -> Result<Self> {
        if !crate::env::is_scoped() {
            return Ok(Self { held: None });
        }
        let space = SpaceSelector::id(space.home()?.id());
        let token = crate::client::spaces::issue_scope(space.clone())
            .send(starting.caller().paths)
            .await?;
        crate::env::act_in_space_scope(token.clone());
        Ok(Self {
            held: Some((space, token)),
        })
    }

    /// Revoke the scope, once the start ended. A token it cannot revoke
    /// goes with the space, as when its removal ended its agent.
    async fn leave(self, starting: &impl Starting) {
        if let Some((space, token)) = self.held {
            let _ = crate::client::spaces::revoke_scope(space, token)
                .send(starting.caller().paths)
                .await;
        }
    }
}
