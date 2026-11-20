//! Arguments of commands Shoal, its agent hooks and integrations run.
use shoal_ids::space_ids::SpaceId;
use std::{ffi::OsString, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{cli::commands, sessions::HostName, workers};
use shoal_protocol::{events::EventCursor, space_names::SpaceSelector};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum InternalCommand {
    /// Stream durable workspace lifecycle events without consuming notifications.
    Events {
        /// Keep the stream open as the daemon records new events.
        #[arg(long)]
        follow: bool,
        /// Replay events after this event ID.
        #[arg(long = "since")]
        after: Option<EventCursor>,
    },
    /// Export the environment, with a new scope token, for processes
    /// started in a space.
    Env {
        space: Option<SpaceSelector>,
        /// Revoke a previously exported scope token.
        #[arg(long)]
        revoke: Option<shoal_protocol::scope_tokens::ScopeToken>,
    },
    /// Print a view's rows for the bare `shoal` picker to reload.
    MenuRows { view: commands::menu::View },
    /// Print the fzf actions that switch the bare `shoal` picker to its next view.
    MenuNext,
    /// Print the `shoal approvals` picker's rows for it to reload.
    #[command(name = workers::APPROVAL_ROWS)]
    ApprovalRows {
        #[arg(long)]
        space: Option<SpaceSelector>,
    },
    /// Decide the request a `shoal approvals` picker row names, and print the outcome.
    #[command(name = workers::APPROVAL_DECIDE)]
    ApprovalDecide {
        decision: crate::cli::args::approvals::Decision,
        row: String,
    },
    /// Send a proxy's report for an agent wrapper of an older release, which
    /// an upgraded daemon's protocol no longer accepts.
    #[command(name = workers::PROXY_REPORT)]
    ProxyReport { report: String },
    /// Report the agent's turn state; Shoal's agent hooks call it.
    AgentState {
        state: crate::state::AgentState,
        /// The integration of an agent whose turn this idle report ends,
        /// which reads how long the agent's prompt cache lives.
        #[arg(long, value_name = "INTEGRATION")]
        ends_turn: Option<crate::ai::IntegrationKind>,
        /// Workspace or chat to report for; defaults to the chat this runs
        /// in, else the caller's own or the current workspace.
        #[arg(long)]
        space: Option<SpaceSelector>,
    },
    /// Report a turn an API error ended, with the failure the hook event of an
    /// agent with this integration names; Shoal's agent hooks call it.
    TurnFailed {
        /// The integration whose hook event stdin carries.
        #[arg(long)]
        integration: crate::ai::IntegrationKind,
        /// Workspace or chat to report for; defaults to the chat this runs
        /// in, else the caller's own or the current workspace.
        #[arg(long)]
        space: Option<SpaceSelector>,
    },
    /// Report the plan that the hook event of an agent with this integration
    /// names after a plan tool call; Shoal's agent hooks call it.
    Plan {
        /// The integration whose hook event stdin carries.
        #[arg(long)]
        integration: crate::ai::IntegrationKind,
        /// Workspace or chat to report for; defaults to the chat this runs
        /// in, else the caller's own or the current workspace.
        #[arg(long)]
        space: Option<SpaceSelector>,
    },
    /// Print the space's new alerts, item activity and leftover processes as
    /// hook context, reading the hook event on stdin and never failing the
    /// agent; Shoal's agent hooks run it after tool calls and when a prompt
    /// is submitted.
    Deliver,
    /// Wait for the next alerts or item activity of the workspace or
    /// chat, print them and exit 2; Shoal's Claude Code `Stop` and `StopFailure`
    /// hooks run it in the background so the update starts the idle agent's
    /// next turn.
    Wake {
        /// The agent's process ID; the hook ends when that process exits.
        #[arg(long)]
        agent: u32,
        /// Workspace or chat whose updates to wait for; defaults to the
        /// agent's own.
        #[arg(long, hide = true)]
        space: Option<SpaceSelector>,
        /// The workspace the wake hook of an earlier release names when an
        /// upgrade hands its wait over to this one.
        #[arg(
            long = "workspace",
            hide = true,
            conflicts_with = "space",
            requires = "resume"
        )]
        handed_over: Option<SpaceSelector>,
        /// Continue a wait across a daemon restart, as the installed release
        /// does after an upgrade restarted the daemon.
        #[arg(long, hide = true)]
        resume: bool,
    },
    /// Print which workspace a starting subagent works in as hook context;
    /// Shoal's Claude Code and Codex `SubagentStart` hooks run it.
    SubagentStart,
    /// Report the start or stop of a subagent, or the session start, that
    /// the hook event of an agent with this integration names; Shoal's agent
    /// hooks call it.
    Subagent {
        /// The integration whose hook event stdin carries.
        #[arg(long)]
        integration: crate::ai::IntegrationKind,
        /// Workspace or chat to report for; defaults to the chat this runs
        /// in, else the caller's own or the current workspace.
        #[arg(long)]
        space: Option<SpaceSelector>,
    },
    /// Create a child workspace for the worktree Claude Code creates and print
    /// its path; Shoal's Claude Code `WorktreeCreate` hook runs it.
    WorktreeCreate,
    /// Mark the child workspace at the worktree Claude Code removes done;
    /// Shoal's Claude Code `WorktreeRemove` hook runs it.
    WorktreeRemove,
    /// Finish a planned merge inside its tracked execution.
    #[command(name = workers::MERGE)]
    Merge {
        /// Where to write what it merged, or why it could not.
        #[arg(long)]
        result: PathBuf,
        plan: String,
    },
    /// Finish a planned unmerge inside its tracked execution.
    #[command(name = workers::UNMERGE)]
    Unmerge {
        /// Where to write what it undid, or why it could not.
        #[arg(long)]
        result: PathBuf,
        plan: String,
    },
    /// Close a terminal host's place once its workspace completes or is removed.
    #[command(name = workers::PLACE_WATCH)]
    PlaceWatch {
        host: HostName,
        workspace: SpaceId,
        place: String,
        /// What told the place apart when the watch began.
        #[arg(long)]
        instance: Option<String>,
        /// The HEAD of the kept completion the watch began under.
        #[arg(long)]
        kept_at: Option<String>,
    },
    /// The Herdr tab watcher of earlier releases, which continues here after
    /// an upgrade.
    #[command(name = workers::HERDR_WATCH)]
    HerdrWatch { workspace: SpaceId, tab: String },
    /// Detached execution wrapper; reports the launch on stdout, then keeps tracking it.
    #[command(name = workers::DETACHED)]
    Detached {
        workspace: SpaceId,
        #[arg(long)]
        log: PathBuf,
        #[arg(long, value_parser = agent)]
        agent: Option<crate::agent::Agent>,
        #[command(flatten)]
        model: crate::cli::args::ModelArgs,
        /// The mode the agent started in.
        #[arg(long, default_value_t = crate::config::modes::ModeName::DEFAULT)]
        mode: crate::config::modes::ModeName,
        /// The `--permission` narrowing it started with, as JSON.
        #[arg(long, value_parser = narrowing, default_value = "{}")]
        narrowing: crate::config::permissions::PermissionsConfig,
        /// The agent begins a new session.
        #[arg(long)]
        new_session: bool,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run a tool with its configured secrets; an agent's secret launchers call it.
    #[command(name = workers::SECRET_TOOL)]
    SecretTool {
        launchers: PathBuf,
        tool: String,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
    /// Git's remote helper for an agent's pushes, which asks the daemon
    /// before pushing; the agent's push helper launcher calls it.
    #[command(name = workers::PUSH_GATE)]
    PushGate { remote: String, url: String },
    /// Git's credential helper for an agent's Git over HTTPS, which asks
    /// the daemon for the host's agent token; the agent's Git settings call
    /// it with Git's operation.
    #[command(name = workers::GIT_CREDENTIAL)]
    GitCredential { operation: String },
    /// Run an operation the daemon started, reading its order on stdin.
    #[command(name = workers::OPERATION)]
    Operation,
    /// Continue the spaces of the package another joined machine moves
    /// here, on stdin, as `shoal restore --from -` does, and print how it
    /// went as JSON; the daemon runs it.
    #[command(name = workers::IMPORT_MOVED)]
    ImportMoved {
        /// The move's ticket, which the claims of its spaces carry.
        #[arg(long)]
        ticket: shoal_ids::move_tickets::MoveTicket,
        /// Leave the agents stopped for a later shoal resume.
        #[arg(long)]
        no_resume: bool,
    },
    /// Attach this terminal, which a user on another machine views, to an
    /// agent's session as `shoal attach` would there; the daemon runs it.
    #[command(name = workers::ATTACH)]
    Attach {
        space: SpaceId,
        /// What the user's terminal asks for, as JSON.
        #[arg(long, value_parser = viewer)]
        viewer: shoal_protocol::attaching::Viewer,
        /// This machine's name.
        #[arg(long)]
        machine: String,
        /// The descriptor to write the chosen session's name to.
        #[arg(long)]
        report: i32,
    },
    /// Start an admitted agent queue entry; the daemon runs it.
    #[command(name = workers::QUEUE_START)]
    QueueStart {
        entry: shoal_ids::queue_entry_ids::QueueEntryId,
    },
    /// Run a recorded space's agent, or a workspace's shell, inside the
    /// place `host` opened for it.
    #[command(name = workers::PLACE)]
    Place(PlaceWorkerArgs),
    /// Tracked agent inside a new place; consumes recovery records once started.
    #[command(name = workers::SESSION)]
    Session {
        space: SpaceId,
        #[arg(value_parser = agent)]
        agent: crate::agent::Agent,
        #[command(flatten)]
        model: crate::cli::args::ModelArgs,
        /// The mode the agent started in.
        #[arg(long, default_value_t = crate::config::modes::ModeName::DEFAULT)]
        mode: crate::config::modes::ModeName,
        /// The `--permission` narrowing it started with, as JSON.
        #[arg(long, value_parser = narrowing, default_value = "{}")]
        narrowing: crate::config::permissions::PermissionsConfig,
        /// The agent begins a new session.
        #[arg(long)]
        new_session: bool,
        #[arg(long = "record")]
        records: Vec<PathBuf>,
        /// The host that opened the place.
        #[arg(long)]
        host: HostName,
        /// Close the place of this host once the workspace is done.
        #[arg(long)]
        close_in: Option<HostName>,
        /// The user restores the agent by hand.
        #[arg(long)]
        manual: bool,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// A demo's wrapper, restarting its command until the demo is stopped.
    #[command(name = workers::DEMO)]
    Demo {
        workspace: SpaceId,
        #[arg(long)]
        launch: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Run the command a launch file carries, deleting the file first; hosts
    /// that cannot set a place's environment type it into the place.
    #[command(name = workers::LAUNCH)]
    Launch { file: PathBuf },
    /// Continue an agent's wrapper from the installed binary, which an
    /// upgrade put in place of its own, over the daemon connection it hands over.
    #[command(name = workers::TAKE_OVER)]
    TakeOver {
        workspace: SpaceId,
        #[arg(long, value_parser = agent)]
        agent: crate::agent::Agent,
        #[command(flatten)]
        model: crate::cli::args::ModelArgs,
        /// The mode the agent started in.
        #[arg(long, default_value_t = crate::config::modes::ModeName::DEFAULT)]
        mode: crate::config::modes::ModeName,
        /// The `--permission` narrowing it started with, as JSON.
        #[arg(long, value_parser = narrowing, default_value = "{}")]
        narrowing: crate::config::permissions::PermissionsConfig,
        /// The descriptor of the wrapper's daemon connection.
        #[arg(long)]
        connection: i32,
        /// The recovery record the agent resumes from.
        #[arg(long)]
        record: Option<PathBuf>,
        /// The log a detached agent writes to.
        #[arg(long)]
        log: Option<PathBuf>,
    },
    /// Copy the main checkout's ignored files inside tracked setup, then run setup.
    #[command(name = workers::COPY_IGNORED)]
    CopyIgnored {
        /// A gitignore-style pattern of entries to leave out; repeatable.
        #[arg(long)]
        exclude: Vec<String>,
        #[arg(last = true)]
        setup_cmd: Option<PathBuf>,
    },
}

/// The agent a worker runs, which Shoal recorded and passes on unchecked.
fn agent(name: &str) -> Result<crate::agent::Agent, String> {
    Ok(crate::agent::Agent::unchecked(name.to_owned()))
}

/// The viewer an attach for another machine receives as JSON.
fn viewer(json: &str) -> Result<shoal_protocol::attaching::Viewer, String> {
    serde_json::from_str(json).map_err(|error| error.to_string())
}

/// A `--permission` narrowing a worker receives as JSON.
fn narrowing(json: &str) -> Result<crate::config::permissions::PermissionsConfig, String> {
    serde_json::from_str(json).map_err(|error| error.to_string())
}

// Arguments of the place worker.
#[derive(Debug, Args)]
pub struct PlaceWorkerArgs {
    /// The host whose place this worker runs in; none for an agent that
    /// runs its own way.
    #[arg(long)]
    pub host: Option<HostName>,
    /// Close the place once its workspace is done.
    #[arg(long)]
    pub close_when_done: bool,
    /// The place was opened focused, showing the space's session.
    #[arg(long)]
    pub focused: bool,
    #[arg(env = crate::env::PLACE_PLAN, hide_env_values = true)]
    pub plan: String,
}
