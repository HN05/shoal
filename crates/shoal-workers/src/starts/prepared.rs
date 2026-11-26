//! Following the preparation of a workspace a start recorded.
use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::Starting;
use crate::{
    agent::Agent,
    client::connection::CommandPaths,
    client::preparation::{self, Preparing, Unprepared},
    hosting,
    model::Workspace,
    operations::error,
    preparation::{SetUp, follow, settle_setup},
    protocol::{
        ConfigTarget,
        operations::{Preparation, Prepared, StartedWorkspace},
    },
    terminal::Owner,
};

/// Follow the `operation` preparing `workspace` for its agent `agent`,
/// returning the workspace once it is prepared, or `None` when it was not.
pub async fn follow_preparation(
    starting: &impl Starting,
    workspace: Workspace,
    operation: String,
    agent: Option<Agent>,
) -> Result<Option<Workspace>> {
    let started = StartedWorkspace {
        operation: Some(operation),
        workspace: Box::new(workspace),
        reused: false,
        entry: None,
        warnings: Vec::new(),
    };
    let preparing = Preparing::follow(starting.caller().paths, started).await?;
    // JSON output reports the workspace once the agent starts, or its
    // waiting entry instead.
    let named = starting.json();
    let finished = finish_workspace(starting, preparing, agent.as_ref(), named).await?;
    Ok(finished.map(|Prepared { workspace, .. }| *workspace))
}

/// Follow the preparation of the started workspace, which sets it up, links
/// its PR and queues its agent, before reporting it, unless the start
/// `named` it, which reports only its agent, and entering it unless `agent`
/// takes the terminal.
pub async fn finish_workspace(
    starting: &impl Starting,
    preparing: Preparing<'_, CommandPaths>,
    agent: Option<&Agent>,
    named: bool,
) -> Result<Option<Prepared>> {
    let reused = preparing.reused();
    let Some(SetUp {
        workspace,
        post_setup,
        agent: entry,
    }) = followed_setup(starting, preparing).await?
    else {
        return Ok(None);
    };
    // The post-setup hook may change how the agent launches, so the shell's
    // destination waits for it; a failed hook starts no agent.
    let (prepared, post_setup) = match post_setup {
        Some(result) => (
            Prepared {
                workspace: Box::new(workspace),
                agent: entry,
            },
            result,
        ),
        None => prepare_after_setup(starting, workspace, reused).await?,
    };
    let workspace = &prepared.workspace;
    if !named {
        starting.prepared(workspace, reused)?;
    }
    let starts = agent.filter(|_| post_setup.is_ok());
    enter_workspace(starting, workspace, starts).await?;
    post_setup?;
    Ok(Some(prepared))
}

/// Follow the preparation of the started workspace and settle a failed
/// setup, which leaves its agent waiting in the queue, as its preparation
/// queued it; a retry runs what follows setup with it.
async fn followed_setup(
    starting: &impl Starting,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Option<SetUp>> {
    let retry = Preparation {
        setup: true,
        post_setup: !preparing.reused(),
        ..Default::default()
    };
    let paths = starting.caller().paths;
    let prepared = follow(paths, starting, preparing).await?;
    settle_setup(paths, starting, prepared, Some(retry)).await
}

/// Run what preparation leaves after setup once the user ignored its
/// failure: the post-setup hook unless the workspace was `reused`.
/// Returns the workspace with its agent, and how the hook went.
async fn prepare_after_setup(
    starting: &impl Starting,
    workspace: Workspace,
    reused: bool,
) -> Result<(Prepared, Result<()>)> {
    let steps = Preparation {
        post_setup: !reused,
        ..Default::default()
    };
    let paths = starting.caller().paths;
    let env = crate::env::without_secrets();
    let preparing = preparation::prepare(paths, &workspace, steps, env).await?;
    let ended = follow(paths, starting, preparing).await?;
    Ok(match ended {
        Ok(prepared) => (prepared, Ok(())),
        Err(Unprepared {
            workspace, failure, ..
        }) => {
            let agent = None;
            (Prepared { workspace, agent }, Err(error(failure)))
        }
    })
}

/// Have the caller's shell enter `workspace` once `agent`, if any, ends,
/// where [`enters`] says it does.
pub async fn enter_workspace(
    starting: &impl Starting,
    workspace: &Workspace,
    agent: Option<&Agent>,
) -> Result<()> {
    if enters(starting, workspace, agent).await?
        && !hosting::enter_directory(starting.caller().paths, &workspace.path)?
    {
        starting.not_integrated();
    }
    Ok(())
}

/// Whether the caller's shell enters `workspace`, where `agent` starts
/// unless `None`. An agent that runs in a session this terminal attaches
/// to leaves it where it was, as detaching returns to that shell rather
/// than to the workspace.
async fn enters(
    starting: &impl Starting,
    workspace: &Workspace,
    agent: Option<&Agent>,
) -> Result<bool> {
    let caller = starting.caller();
    if !hosting::follows_caller(caller) {
        return Ok(false);
    }
    let Some(chosen) = agent else {
        return Ok(true);
    };
    let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
    let settings = crate::settings::of(caller.paths, target).await?;
    Ok(!crate::launch::runs_in_session(chosen, &settings))
}

/// End the start of `workspace`, whose preparation failed, leaving its
/// agent, if `with_agent`, waiting in the queue for a retried setup, and
/// the place opened for it hosting the workspace meanwhile.
pub async fn unprepared(
    starting: &impl Starting,
    workspace: &Workspace,
    with_agent: bool,
) -> Result<i32> {
    let owner = Owner::Workspace(&workspace.id);
    let name = workspace.name.as_str();
    hosting::host_space(starting.caller(), owner, with_agent, name).await?;
    Ok(1)
}

/// Report the prepared `workspace`, opened without an agent, which JSON
/// output reports once prepared, and give it a shell in the place opened
/// for it, which hosts the workspace; the caller's own terminal gets its
/// shell back.
pub async fn shell_in(starting: &impl Starting, workspace: &Workspace) -> Result<i32> {
    if starting.json() {
        shoal_core::outln!("{}", crate::client::output::json(workspace)?);
    }
    let caller = starting.caller();
    let owner = Owner::Workspace(&workspace.id);
    hosting::host_space(caller, owner, false, workspace.name.as_str()).await?;
    hosting::open_shell(caller, &workspace.path).await
}
