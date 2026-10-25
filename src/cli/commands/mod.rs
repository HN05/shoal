//! CLI dispatch. Domain handlers own requests, prompts, and rendering;
//! daemon modules own lifecycle and allocation policy.
mod acquisition;
mod agent_queue;
mod agent_state;
mod api;
mod approvals;
mod base;
mod branches;
mod capabilities;
mod chat;
mod cleanup;
mod configuration;
pub(crate) mod configured;
mod contribute;
mod events;
mod holds;
mod inbox;
pub(super) mod issues;
mod items;
mod labels;
mod leases;
mod links;
mod listing;
pub(crate) mod menu;
mod messages;
mod notifications;
mod plan;
mod ports;
mod questions;
mod recovery;
mod repo_menu;
mod repo_migrate;
mod repo_setup;
mod repositories;
mod resources;
mod restore;
pub(crate) mod resume;
mod review;
mod secrets;
mod service;
mod sessions;
mod simulators;
mod skill;
mod split;
mod stop;
mod transfer;
mod update;
mod view;
pub(in crate::cli) mod workspaces;

#[cfg(test)]
mod tests;

pub(in crate::cli) use approvals::summary as approval_summary;
pub(in crate::cli) use questions::summary as question_summary;

use anyhow::{Result, ensure};
use clap::CommandFactory;
use futures_util::{StreamExt, stream};
use serde::Serialize;
use serde_json::json;

use crate::{
    agent::CodexMode,
    cli::{
        ApprovalsCommand, Cli, Command, ConfigCommand, ExportArgs, ImportArgs, InternalCommand,
        NotificationsCommand, QuestionsCommand, ShellCommand, WorkCommand, WorkspaceScope, agents,
        client,
        context::Context,
        output::{Palette, Style},
    },
    daemon::approvals::proposals::{Duplicate, ProposalDecision},
    env,
    forge::items::link::ItemKind,
    model::Workspace,
    paths::Paths,
    shell,
};

#[derive(Serialize)]
#[serde(untagged)]
enum WorkspaceOverviewResult<T> {
    Ready(T),
    Failed {
        workspace: Box<crate::model::Workspace>,
        error: String,
    },
}

impl<T> WorkspaceOverviewResult<T> {
    fn failed(workspace: crate::model::Workspace, error: impl std::fmt::Display) -> Self {
        Self::Failed {
            workspace: Box::new(workspace),
            error: error.to_string(),
        }
    }

    fn is_failed(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Collect every workspace before reporting partial failures. Domains retain
/// their successful JSON payload and resource-specific text rendering.
async fn workspace_overviews<T: Serialize>(
    ctx: &Context,
    request: impl AsyncFn(&Workspace) -> Result<T>,
    render: impl Fn(&T, Palette),
) -> Result<i32> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let overviews = collect_workspace_overviews(&workspaces, request).await;
    let failed = overviews.iter().any(WorkspaceOverviewResult::is_failed);
    ctx.show(&overviews, |overviews| {
        let palette = Palette::stdout(ctx.json);
        for (workspace, overview) in workspaces.iter().zip(overviews) {
            let heading = palette.paint(Style::Heading, &workspace.name);
            match overview {
                WorkspaceOverviewResult::Ready(overview) => {
                    outln!("{heading}");
                    render(overview, palette);
                }
                WorkspaceOverviewResult::Failed { error, .. } => {
                    outln!("{heading}: {}", palette.paint(Style::Warning, error));
                }
            }
        }
    })?;
    Ok(i32::from(failed))
}

// Keep concurrent reads modest: each request opens several SQLite connections.
const WORKSPACE_OVERVIEW_CONCURRENCY: usize = 2;

async fn collect_workspace_overviews<T>(
    workspaces: &[Workspace],
    request: impl AsyncFn(&Workspace) -> Result<T>,
) -> Vec<WorkspaceOverviewResult<T>> {
    let mut pending = stream::iter(workspaces.iter().enumerate())
        .map(|(index, workspace)| {
            let request = &request;
            async move {
                let result = match request(workspace).await {
                    Ok(overview) => WorkspaceOverviewResult::Ready(overview),
                    Err(error) => {
                        WorkspaceOverviewResult::failed(workspace.clone(), format!("{error:#}"))
                    }
                };
                (index, result)
            }
        })
        .buffer_unordered(WORKSPACE_OVERVIEW_CONCURRENCY);
    let mut overviews: Vec<_> = std::iter::repeat_with(|| None)
        .take(workspaces.len())
        .collect();
    while let Some((index, overview)) = pending.next().await {
        overviews[index] = Some(overview);
    }
    overviews.into_iter().map(Option::unwrap).collect()
}

/// Subcommand path of an invocation that opens a menu; without a terminal its
/// help is printed instead.
/// The view the picker opens on for `command`: bare `shoal` lets it choose,
/// and a listing names its own unless it prints. `None` runs the command.
fn picker_start(ctx: &Context, command: Option<&Command>) -> Option<Option<menu::View>> {
    match command {
        None => Some(None),
        // Workspace processes, who cannot act on it, get the inbox printed.
        Some(Command::Inbox {
            repository: None,
            print: false,
        }) if lists_interactively(ctx) && !env::inherits_scope(&ctx.paths.state) => {
            Some(Some(menu::View::Inbox))
        }
        Some(Command::Work {
            command: None,
            queue: false,
            chats: false,
            repository: None,
            status,
            print: false,
        }) if status.is_empty() && lists_interactively(ctx) => Some(Some(menu::View::Work)),
        _ => None,
    }
}

/// Whether a listing opens the picker: on a terminal, unless its output is
/// redirected to keep the list.
fn lists_interactively(ctx: &Context) -> bool {
    ctx.interactive() && std::io::IsTerminal::is_terminal(&std::io::stdout())
}

fn menu_path(command: Option<&Command>) -> Option<&'static [&'static str]> {
    match command {
        None => Some(&[]),
        Some(Command::Repo { command: None }) => Some(&["repo"]),
        Some(_) => None,
    }
}

fn print_help(path: &[&str]) -> Result<()> {
    let mut root = Cli::command();
    root.build();
    let mut command = &mut root;
    for name in path {
        command = command
            .find_subcommand_mut(name)
            .expect("menu subcommands are defined");
    }
    out!("{}", Palette::stdout(false).styled(&command.render_help()));
    Ok(())
}

pub(crate) async fn run(cli: Cli) -> Result<i32> {
    if matches!(cli.command, Some(Command::BuildInfo)) {
        outln!("{}", json!({"protocol": crate::protocol::VERSION}));
        return Ok(0);
    }
    // The service runs without the user's tool configuration environment, and
    // skill installation does this work itself.
    if !matches!(
        cli.command,
        Some(
            Command::Daemon {
                command: crate::cli::DaemonCommand::Run { .. }
            } | Command::Skill {
                command: Some(_),
                ..
            }
        )
    ) && let Ok(state) = Paths::state_dir(cli.state_dir.clone())
    {
        skill::refresh_installed(&state, cli.json);
    }
    // Skill delivery is independent of daemon state and socket-path limits.
    if let Some(Command::Skill { name, command }) = &cli.command {
        return skill::run(*name, command.as_ref(), cli.json, cli.state_dir.clone());
    }
    // Shell recovery must also work after cleanup, without daemon configuration.
    if let Some(Command::Shell {
        command: ShellCommand::Recover { path },
    }) = &cli.command
    {
        let state = crate::paths::Paths::state_dir(cli.state_dir.clone())?;
        let destination = shell::recovery_directory(path, &state)?;
        if cli.json {
            outln!("{}", json!({"path": destination}));
        } else {
            outln!("{}", destination.display());
        }
        return Ok(0);
    }
    if let Some(path) = menu_path(cli.command.as_ref())
        && !Context::is_interactive(cli.json)
    {
        print_help(path)?;
        return Ok(0);
    }
    let mut ctx = Context::new(Paths::new(cli.state_dir)?, cli.json);
    ctx.need = cli.need;
    // The daemon pins its own config's forges and keeps them across a
    // reload that fails, which a loader reading the file later would not.
    if !matches!(
        cli.command,
        Some(Command::Daemon {
            command: crate::cli::DaemonCommand::Run { .. }
        })
    ) {
        ctx.learn_pinned_forges();
    }
    let command = match picker_start(&ctx, cli.command.as_ref()) {
        Some(start) => match menu::choose(&ctx, start).await? {
            menu::Choice::Run(command) => *command,
            menu::Choice::Done(code) => return Ok(code),
        },
        None => match cli.command {
            Some(Command::Repo { command: None }) => repo_menu::choose(&ctx).await?,
            Some(command) => command,
            None => unreachable!("bare shoal opens the picker"),
        },
    };
    ctx.recent = command.recent_name();
    ensure!(
        !(command.is_administrative() && env::inherits_scope(&ctx.paths.state)),
        "workspace processes cannot administer Shoal; ask the user to run this command"
    );
    match command {
        Command::BuildInfo => unreachable!("build metadata needs no daemon or paths"),
        Command::Run {
            name,
            workspace,
            args,
        } => match name {
            Some(name) => agents::run_named(&ctx, &name, workspace, args).await,
            None => configured::list(&ctx).await,
        },
        Command::Custom(words) => match configured::invocation(words)? {
            Some((name, workspace, args)) => agents::run_named(&ctx, &name, workspace, args).await,
            None => Ok(0),
        },
        Command::Skill { name, command } => skill::run(
            name,
            command.as_ref(),
            ctx.json,
            Some(ctx.paths.state.clone()),
        ),
        Command::Completions { shell } => {
            let script = shell::completions(shell)?;
            ctx.emit(&script, json!({"script": script}))?;
            Ok(0)
        }
        Command::Shell {
            command: ShellCommand::Init,
        } => {
            ctx.emit(shell::INIT, json!({"script": shell::INIT}))?;
            Ok(0)
        }
        Command::Shell {
            command: ShellCommand::Recover { .. },
        } => unreachable!("shell recovery is handled before loading paths"),
        Command::Repo {
            command: Some(command),
        } => repositories::run(&ctx, command).await,
        Command::Repo { command: None } => unreachable!("the repository menu returns a command"),
        Command::Add {
            here,
            focus,
            path,
            repository,
            repository_override,
            branch,
            existing,
            issue,
            base,
            git_profile,
            agent,
            now,
            after,
            args,
        } => {
            links::add(
                &ctx,
                links::AddInput {
                    positional: repository,
                    repository: repository_override,
                    issue,
                },
                workspaces::Creation {
                    path,
                    branch,
                    existing,
                    base,
                    git_profile,
                    pr: None,
                },
                agent,
                args,
                workspaces::AddOptions {
                    here,
                    focus,
                    now,
                    after,
                },
            )
            .await
        }
        Command::Contribute(args) => contribute::contribute(&ctx, args).await,
        Command::Pr { command } => items::pr(&ctx, command).await,
        Command::Ci { command } => items::ci(&ctx, command).await,
        Command::Issue { command } => items::issue(&ctx, command).await,
        Command::Link { items } => links::link(&ctx, items).await,
        Command::Unlink { items } => links::unlink(&ctx, items).await,
        Command::View {
            items,
            no_comments,
            check,
        } => view::view(&ctx, items, no_comments, check).await,
        Command::Watch {
            items,
            timeout,
            resume,
        } => links::watch(&ctx, items, timeout, resume).await,
        Command::Labels(repository) => labels::labels(&ctx, repository.workspace).await,
        Command::Capabilities(repository) => {
            capabilities::capabilities(&ctx, repository.workspace).await
        }
        Command::Api(args) => api::api(&ctx, args).await,
        Command::Ready { items } => links::ready(&ctx, items).await,
        Command::Unready {
            items,
            draft,
            no_draft,
        } => links::unready(&ctx, items, (draft || no_draft).then_some(draft)).await,
        Command::Adopt {
            path,
            repository,
            copy,
        } => workspaces::adopt(&ctx, path, repository, copy).await,
        Command::Rename { workspace, branch } => workspaces::rename(&ctx, workspace, branch).await,
        Command::Split {
            branch,
            issue,
            base,
            workspace,
            agent,
            args,
        } => {
            let split = split::Split {
                branch,
                issue,
                base,
                agent,
                args,
            };
            split::run(&ctx, workspace, split).await
        }
        Command::Internal { command } => internal(ctx, command).await,
        Command::Inbox {
            repository,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, vec![]).await?;
            inbox::list(&ctx, &filter).await
        }
        Command::Work {
            command: Some(WorkCommand::Setup { workspace }),
            ..
        } => workspaces::setup(&ctx, workspace).await,
        Command::Work {
            command: None,
            queue,
            chats,
            repository,
            status,
            print: _,
        } => {
            let filter = listing::ListFilter::resolve(&ctx, repository, status).await?;
            if chats {
                return chat::list(&ctx, &filter).await;
            }
            workspaces::list(&ctx, queue, &filter).await
        }
        Command::Hold { command, scope } => holds::run(&ctx, command, scope).await,
        Command::Base { command, workspace } => base::run(&ctx, command, workspace).await,
        Command::Status { workspace, item } => workspaces::status(&ctx, workspace, item).await,
        Command::Cd { workspace } => workspaces::cd(&ctx, workspace).await,
        Command::Edit { workspace, args } => workspaces::edit(&ctx, workspace, args).await,
        Command::Attach { workspace, session } => sessions::attach(&ctx, workspace, session).await,
        Command::Diff { workspace } => workspaces::diff(&ctx, workspace).await,
        Command::Conflicts { target, workspace } => {
            workspaces::conflicts(&ctx, workspace, target).await
        }
        Command::Review {
            workspace,
            pr,
            issue,
            repository,
            post,
            no_post,
            manual,
            agent,
            args,
        } => {
            let item = pr
                .map(|pr| (ItemKind::Pr, pr))
                .or(issue.map(|issue| (ItemKind::Issue, issue)));
            let post = (post || no_post).then_some(post);
            let target = review::Target::new(workspace, item, repository, post)?;
            let reviewer = review::Reviewer::new(manual, agent);
            review::start(&ctx, target, reviewer, args).await
        }
        Command::Sync { repository } => repositories::sync(&ctx, repository).await,
        Command::Land {
            workspace,
            push,
            no_push,
        } => workspaces::land(&ctx, workspace, (push || no_push).then_some(push)).await,
        Command::Undone { workspace } => workspaces::undone(&ctx, workspace).await,
        Command::Notify { message, workspace } => {
            notifications::send(&ctx, workspace, message).await
        }
        Command::Message {
            message,
            body_file,
            workspace,
        } => messages::send(&ctx, workspace, message, body_file).await,
        Command::Ask {
            question,
            recommend,
            reason,
            choices,
            wait,
            workspace,
            resume,
        } => {
            let ask = crate::daemon::questions::Ask {
                question,
                recommendation: recommend,
                reason,
                choices,
            };
            // Clap requires --workspace with --resume.
            match (workspace, resume) {
                (Some(workspace), Some(id)) => questions::resume(&ctx, workspace, id, ask).await,
                (workspace, _) => questions::ask(&ctx, workspace, ask, wait).await,
            }
        }
        Command::Questions {
            command: Some(QuestionsCommand::Withdraw { id, workspace }),
            ..
        } => questions::withdraw(&ctx, workspace, id).await,
        Command::Questions {
            command: None,
            workspace,
        } => questions::list(&ctx, workspace).await,
        Command::Answer { id, answer } => questions::answer(&ctx, id, answer).await,
        Command::Messages { workspace, hook } => {
            if hook {
                messages::hook(&ctx).await
            } else {
                messages::show(&ctx, workspace).await
            }
        }
        Command::Done { workspace, cleanup } => workspaces::done(&ctx, workspace, cleanup).await,
        Command::Env { workspace, revoke } => {
            workspaces::environment(&ctx, workspace, revoke).await
        }
        Command::Inspect { workspace } => workspaces::inspect(&ctx, workspace).await,
        Command::Notifications {
            command: Some(NotificationsCommand::Clear),
            ..
        } => notifications::clear(&ctx).await,
        Command::Notifications {
            command: Some(NotificationsCommand::Withdraw { id, workspace }),
            ..
        } => notifications::withdraw(&ctx, workspace, id).await,
        Command::Notifications {
            command: None,
            follow: true,
            keep_unread,
            resume,
            ..
        } => notifications::follow(&ctx, keep_unread, resume.unwrap_or(0)).await,
        Command::Notifications {
            command: None,
            all,
            keep_unread,
            limit,
            ..
        } => notifications::list(&ctx, all, keep_unread, limit).await,
        Command::Resume {
            scope,
            execution,
            discard,
        } => resume::select(&ctx, scope, execution, discard).await,
        Command::Stop { scope } => stop::run(&ctx, scope).await,
        Command::Rm {
            workspace,
            confirmation,
        } => workspaces::remove(&ctx, workspace, confirmation.yes).await,
        Command::Cleanup { dry_run, caches } => cleanup::run(&ctx, dry_run, caches).await,
        Command::Branch { command } => branches::run(&ctx, command).await,
        Command::Checkpoint { workspace, name } => restore::checkpoint(&ctx, workspace, name).await,
        Command::Restore {
            workspace,
            checkpoint,
        } => restore::run(&ctx, workspace, checkpoint).await,
        Command::Export(ExportArgs {
            repository: Some(repository),
            output,
            confirmation,
            ..
        }) => transfer::export_all(&ctx, repository, output, confirmation.yes).await,
        Command::Export(ExportArgs {
            workspace,
            repository: None,
            output,
            confirmation,
            ..
        }) => transfer::export(&ctx, workspace, output, confirmation.yes).await,
        Command::Import(ImportArgs {
            files,
            repository,
            keep_sessions,
            new_sessions,
        }) => {
            let sessions = if keep_sessions {
                Some(true)
            } else if new_sessions {
                Some(false)
            } else {
                None
            };
            let options = transfer::ImportOptions {
                repository,
                sessions,
            };
            transfer::import(&ctx, files, options).await
        }
        Command::Exec { workspace, command } => workspaces::exec(&ctx, workspace, command).await,
        Command::Claude { workspace, args } => {
            agents::run_named(&ctx, "claude", workspace, args).await
        }
        Command::Codex {
            workspace,
            cli,
            app,
            args,
        } => {
            let mode = if cli {
                Some(CodexMode::Cli)
            } else if app {
                Some(CodexMode::App)
            } else {
                None
            };
            agents::codex(&ctx, mode, workspace, args).await
        }
        Command::T3 { workspace, args } => agents::open_app(&ctx, workspace, "t3", args).await,
        #[cfg(feature = "happy")]
        Command::Happy {
            agent,
            workspace,
            prompt,
            args,
        } => agents::happy(&ctx, agent, workspace, prompt, args).await,
        Command::Chat {
            here,
            repository,
            agent,
            args,
        } => chat::start(&ctx, repository, agent, here, args).await,
        Command::Plan {
            issue,
            oldest,
            newest,
            repository,
            here,
            agent,
            args,
        } => {
            let pick = match (oldest, newest) {
                (true, _) => plan::Pick::Oldest,
                (_, true) => plan::Pick::Newest,
                _ => plan::Pick::Picker,
            };
            plan::start(&ctx, issue, repository, pick, agent, here, args).await
        }
        Command::Acquire { kind, workspace } => leases::acquire(&ctx, kind, workspace).await,
        Command::Release { kind, workspace } => leases::release(&ctx, kind, workspace).await,
        Command::Leases {
            kind,
            workspace,
            all,
        } => leases::show(&ctx, kind, WorkspaceScope { workspace, all }).await,
        Command::Port { command, scope } => ports::run(&ctx, command, scope).await,
        Command::Approvals {
            command: Some(ApprovalsCommand::Withdraw { id, workspace }),
            ..
        } => approvals::withdraw(&ctx, workspace, id).await,
        Command::Approvals {
            command: None,
            workspace,
        } => approvals::list(&ctx, workspace).await,
        Command::Approve { id, title, body } => {
            let details = ProposalDecision {
                title,
                body: items::read_body(body)?,
                ..ProposalDecision::default()
            };
            approvals::decide(&ctx, id, true, details).await
        }
        Command::Deny {
            id,
            reason,
            duplicate_of,
            duplicate_of_approval,
        } => {
            let duplicate_of = duplicate_of
                .map(Duplicate::Issue)
                .or(duplicate_of_approval.map(Duplicate::Approval));
            let details = ProposalDecision {
                reason,
                duplicate_of,
                ..ProposalDecision::default()
            };
            approvals::decide(&ctx, id, false, details).await
        }
        Command::Escalate { id } => approvals::escalate(&ctx, id).await,
        Command::Resource { command, scope } => resources::run(&ctx, command, scope).await,
        Command::Sim { command, scope } => simulators::run(&ctx, command, scope).await,
        Command::Doctor {
            scope,
            repair,
            stop,
            acknowledge_stopped,
            reclaim,
        } => {
            let options = crate::daemon::recovery::ReconcileOptions {
                repair,
                stop,
                acknowledge_stopped,
                reclaim,
            };
            recovery::run(&ctx, scope, options).await
        }
        Command::Config { command } => match command {
            ConfigCommand::Set {
                assignments,
                repository,
            } => configuration::edit(&ctx, configuration::sets(assignments)?, repository).await,
            ConfigCommand::Unset { keys, repository } => {
                configuration::edit(&ctx, configuration::unsets(keys), repository).await
            }
            ConfigCommand::Show { workspace } => configuration::show(&ctx, workspace).await,
            ConfigCommand::Reset => service::install_config(&ctx, None).await,
            ConfigCommand::Install { name } => {
                let name = service::packaged_config(&ctx, name)?;
                service::install_config(&ctx, Some(name)).await
            }
        },
        Command::Install {
            dry_run,
            executable,
        } => service::install(&ctx, dry_run, executable).await,
        Command::Update => update::run(&ctx).await,
        Command::Daemon { command } => service::run(ctx, command, cli.daemon_handoff).await,
    }
}

/// Commands Shoal, its agent hooks and integrations run.
async fn internal(ctx: Context, command: InternalCommand) -> Result<i32> {
    match command {
        InternalCommand::Events { follow, since } => events::run(&ctx, follow, since).await,
        InternalCommand::MenuRows { view } => menu::print_rows(&ctx, view).await,
        InternalCommand::MenuNext => menu::print_next(&ctx),
        InternalCommand::AgentState { state, workspace } => {
            agent_state::run(&ctx, workspace, state).await
        }
        InternalCommand::Wake { agent } => messages::wake(&ctx, agent).await,
        InternalCommand::Herdr {
            close_when_done,
            plan,
        } => super::terminal::herdr_worker(ctx, close_when_done, &plan).await,
        InternalCommand::HerdrWatch { workspace, tab } => {
            super::terminal::watch_herdr_tab(&ctx, workspace, tab).await
        }
        InternalCommand::Land { push, plan } => workspaces::land_worker(&ctx, plan, push).await,
        InternalCommand::CopyIgnored { setup_cmd } => {
            workspaces::copy_ignored_worker(&ctx, setup_cmd).await
        }
        InternalCommand::Detached {
            workspace,
            log,
            agent,
            command,
        } => {
            crate::execution::run_detached_wrapper(&ctx.paths, workspace, log, command, agent).await
        }
        InternalCommand::SecretTool {
            launchers,
            tool,
            args,
        } => crate::secrets::run_tool(&ctx.paths, &launchers, &tool, args).await,
        InternalCommand::QueueStart { entry } => agent_queue::start(&ctx, entry).await,
        InternalCommand::Add { plan } => super::terminal::session_worker(ctx, &plan).await,
        InternalCommand::Session {
            workspace,
            agent,
            records,
            command,
        } if records.is_empty() => {
            crate::execution::run(&ctx.paths, workspace, command, Some(agent)).await
        }
        InternalCommand::Session {
            workspace,
            agent,
            records,
            command,
        } => crate::execution::run_recovery(&ctx.paths, workspace, command, agent, records).await,
    }
}

/// Exit status for a request the daemon declined because capacity is busy.
pub(crate) const EXIT_BUSY: i32 = 2;
