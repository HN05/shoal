mod cleanup;
mod cli;
mod client;
mod config;
mod daemon;
mod diff;
mod execution;
mod model;
mod paths;
mod ports;
mod processes;
mod protocol;
mod removal;
mod repo_config;
mod repo_git;
mod repository;
mod resources;
mod scope;
mod service;
mod shell;
mod sim_audit;
mod simctl;
mod simulators;
mod state;
mod store;
mod ui;
mod workspace;
mod worktrunk;

use anyhow::{Result, ensure};
use clap::{CommandFactory, Parser};
use serde_json::json;

use cli::{Cli, Command, DaemonCommand, PortCommand, RepoCommand, ShellCommand};
use paths::Paths;
use protocol::{Body, Method};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let json_output = cli.json;
    match run(cli).await {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            if json_output {
                eprintln!(
                    "{}",
                    json!({"error": {"code": "command_failed", "message": format!("{error:#}")}})
                );
            } else {
                eprintln!("error: {error:#}");
            }
            std::process::exit(1);
        }
    }
}

async fn run(cli: Cli) -> Result<i32> {
    if cli.command.is_none() && !ui::is_interactive(cli.json) {
        Cli::command().print_help()?;
        return Ok(0);
    }
    let paths = Paths::new(cli.state_dir)?;
    let command = match cli.command {
        Some(command) => command,
        None => ui::workspace_menu(&paths).await?,
    };
    if std::env::var_os("SHOAL_SCOPE_TOKEN").is_some() {
        ensure!(
            !matches!(
                &command,
                Command::Setup { .. }
                    | Command::Daemon {
                        command: DaemonCommand::Run { .. }
                            | DaemonCommand::Start
                            | DaemonCommand::Stop
                            | DaemonCommand::Restart
                    }
            ),
            "workspace processes cannot administer Shoal"
        );
    }
    match command {
        Command::Resource { command } => return resource_command(&paths, command, cli.json).await,
        Command::Resources { workspace } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let Body::ResourceOverview(overview) =
                client::call(&paths, Method::ResourceOverview { workspace }).await?
            else {
                anyhow::bail!("unexpected resource overview response");
            };
            if cli.json {
                println!("{}", serde_json::to_string(&overview)?);
            } else {
                for pool in &overview.pools {
                    println!(
                        "{} ({}) {}/{} in use, {} available{}",
                        pool.name,
                        pool.scope,
                        pool.used,
                        pool.capacity,
                        pool.available,
                        if pool.configuration_matches {
                            ""
                        } else {
                            " [configuration changed; drain leases first]"
                        }
                    );
                    for resource in &pool.resources {
                        if resource.kind == resources::ResourceKind::Rwlock {
                            println!(
                                "  {}: {} readers, {} writers; read available: {}, write available: {}",
                                resource.name,
                                resource.readers,
                                resource.writers,
                                resource.read_available,
                                resource.write_available
                            );
                        } else {
                            println!(
                                "  {}: {}/{} in use, {} available",
                                resource.name, resource.used, resource.capacity, resource.available
                            );
                        }
                    }
                }
                for lease in &overview.leases {
                    println!(
                        "  lease {}/{} -> {} [{}]{}",
                        lease.pool,
                        lease.name,
                        lease.resource,
                        lease.mode,
                        lease
                            .reason
                            .as_ref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    );
                }
                if overview.pools.is_empty() && overview.leases.is_empty() {
                    println!("No configured resources or leases");
                }
            }
        }
        Command::Sim { command } => return sim_command(&paths, command, cli.json).await,
        Command::Pull { workspace } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let Body::PulledMain(result) =
                client::call(&paths, Method::PullMain { workspace }).await?
            else {
                anyhow::bail!("unexpected pull response");
            };
            if cli.json {
                println!("{}", serde_json::to_string(&result)?);
            } else if result.updated {
                println!("Updated main to {}", result.commit);
            } else {
                println!("Main is already up to date ({})", result.commit);
            }
        }
        Command::Diff { workspace } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let base = match client::call(&paths, Method::DiffBase { workspace }).await? {
                Body::DiffBase(base) => base,
                _ => anyhow::bail!("unexpected diff base response"),
            };
            return execution::run(
                &paths,
                base.workspace_id,
                vec!["git".into(), "diff".into(), base.commit.into(), "--".into()],
            )
            .await;
        }
        Command::Port { command } => match command {
            PortCommand::Reserve {
                name,
                workspace,
                mut port,
                mut env,
                mut reason,
                on_conflict,
            } => {
                let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
                loop {
                    match client::call(
                        &paths,
                        Method::ReservePort {
                            workspace: workspace.clone(),
                            name: name.clone(),
                            port,
                            env_var: env.clone(),
                            reason: reason.clone(),
                            on_conflict,
                        },
                    )
                    .await?
                    {
                        Body::Port(reservation) => {
                            output(
                                cli.json,
                                &format!(
                                    "{}={} ({})",
                                    reservation.name, reservation.port, reservation.env_var
                                ),
                                serde_json::to_value(reservation)?,
                            );
                            break;
                        }
                        Body::PortSuggestion(proposal) => {
                            if cli.json || !ui::is_interactive(cli.json) {
                                let mut value = serde_json::to_value(&proposal)?;
                                value["reserved"] = json!(false);
                                output(
                                    cli.json,
                                    &format!(
                                        "{}: port {} unavailable; suggested {}. Accept with --port {}",
                                        proposal.name,
                                        proposal.requested_port,
                                        proposal.suggested_port,
                                        proposal.suggested_port
                                    ),
                                    value,
                                );
                                return Ok(2);
                            }
                            println!(
                                "{}: port {} unavailable; suggested {}",
                                proposal.name, proposal.requested_port, proposal.suggested_port
                            );
                            let answer = ui::pick(
                                "Reserve suggested port? ",
                                vec![
                                    ("no".into(), "Cancel".into()),
                                    ("yes".into(), format!("Reserve {}", proposal.suggested_port)),
                                ],
                                false,
                            )?;
                            if answer != "yes" {
                                return Ok(2);
                            }
                            port = Some(proposal.suggested_port);
                            env = Some(proposal.env_var);
                            reason = proposal.reason;
                        }
                        _ => anyhow::bail!("unexpected port response"),
                    }
                }
            }
            PortCommand::List { workspace, all } => {
                let workspace = if all {
                    None
                } else {
                    Some(ui::workspace(&paths, workspace, true, cli.json).await?)
                };
                let ports = match client::call(&paths, Method::Ports { workspace }).await? {
                    Body::Ports(ports) => ports,
                    _ => anyhow::bail!("unexpected port list response"),
                };
                if cli.json {
                    println!("{}", serde_json::to_string(&ports)?);
                } else {
                    let workspaces = ui::workspaces(&paths).await?;
                    for port in ports {
                        let owner = workspaces
                            .iter()
                            .find(|w| w.id == port.workspace_id)
                            .map(|w| w.name.as_str())
                            .unwrap_or(&port.workspace_id);
                        println!(
                            "{owner}/{}={} ({}){}",
                            port.name,
                            port.port,
                            port.env_var,
                            port.reason
                                .as_ref()
                                .map(|r| format!("  {r}"))
                                .unwrap_or_default()
                        );
                    }
                }
            }
            PortCommand::Release { name, workspace } => {
                let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
                client::call(&paths, Method::ReleasePort { workspace, name }).await?;
                output(
                    cli.json,
                    "Port reservation released",
                    json!({"released":true}),
                );
            }
        },
        Command::Ports { workspace } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let overview = match client::call(&paths, Method::PortOverview { workspace }).await? {
                Body::PortOverview(overview) => overview,
                _ => anyhow::bail!("unexpected port overview response"),
            };
            if cli.json {
                println!("{}", serde_json::to_string(&overview)?);
            } else {
                for p in &overview.reserved {
                    println!(
                        "{}={} ({}){}",
                        p.name,
                        p.port,
                        p.env_var,
                        p.reason
                            .as_ref()
                            .map(|r| format!("  {r}"))
                            .unwrap_or_default()
                    );
                }
                for (name, definition) in &overview.configured {
                    if !overview.reserved.iter().any(|p| &p.name == name) {
                        println!(
                            "{name}: not reserved (preferred: {}; conflicts: {:?})",
                            definition
                                .port
                                .map(|p| p.to_string())
                                .unwrap_or_else(|| "automatic".into()),
                            definition.on_conflict.unwrap_or(overview.on_conflict)
                        );
                    }
                }
                if overview.reserved.is_empty() && overview.configured.is_empty() {
                    println!("No configured or reserved ports");
                }
            }
        }
        Command::Cd { workspace } => {
            if workspace.as_deref() == Some("-") {
                let destination = shell::previous_directory()?;
                if std::env::var_os("SHOAL_SCOPE_TOKEN").is_some() {
                    let workspaces = ui::workspaces(&paths).await?;
                    ensure!(
                        workspaces.iter().any(|w| std::fs::canonicalize(&w.path)
                            .is_ok_and(|root| destination.starts_with(root))),
                        "workspace processes cannot navigate outside their worktree"
                    );
                }
                output(
                    cli.json,
                    &destination.display().to_string(),
                    json!({"path": destination}),
                );
                shell::navigate(&destination, cli.json)?;
            } else {
                let workspace = match workspace {
                    Some(workspace) => workspace,
                    None => ui::workspace_picker(&paths, cli.json).await?,
                };
                enter_workspace(&paths, workspace, cli.json).await?;
            }
        }
        Command::Shell {
            command: ShellCommand::Init,
        } => {
            if cli.json {
                println!("{}", json!({"script": shell::INIT}));
            } else {
                print!("{}", shell::INIT);
            }
        }
        Command::Repo {
            command: RepoCommand::Add { source, name },
        } => {
            let source = ui::repository_selector(source)?;
            match client::call(&paths, Method::Register { source, name }).await? {
                Body::Repository(repo) => output(
                    cli.json,
                    &format!("Registered {}", ui::repository_label(&repo)),
                    serde_json::to_value(&repo)?,
                ),
                _ => anyhow::bail!("unexpected registration response"),
            }
        }
        Command::Repo {
            command: RepoCommand::List,
        } => {
            let repos = ui::repositories(&paths).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&repos)?);
            } else {
                for repo in repos {
                    println!("{}", ui::repository_label(&repo));
                }
            }
        }
        Command::Repo {
            command: RepoCommand::Rename { repository, name },
        } => {
            let repository = ui::repository_selector(repository)?;
            match client::call(&paths, Method::RenameRepository { repository, name }).await? {
                Body::Repository(repo) => output(
                    cli.json,
                    &format!("Renamed {}", ui::repository_label(&repo)),
                    serde_json::to_value(&repo)?,
                ),
                _ => anyhow::bail!("unexpected repository response"),
            }
        }
        Command::Add {
            repository,
            name,
            base,
        } => {
            let repository = match repository {
                Some(repo) => ui::repository_selector(repo)?,
                None => ui::pick(
                    "Repository> ",
                    ui::repository_choices(ui::repositories(&paths).await?).await?,
                    cli.json,
                )?,
            };
            let name = match name {
                Some(name) => name,
                None => ui::input("Workspace name", cli.json)?,
            };
            match client::call(
                &paths,
                Method::Add {
                    repository,
                    name,
                    base,
                },
            )
            .await?
            {
                Body::Workspace(workspace) => {
                    output(
                        cli.json,
                        &format!("Created {} at {}", workspace.name, workspace.path.display()),
                        serde_json::to_value(&workspace)?,
                    );
                    shell::navigate(&workspace.path, cli.json)?;
                }
                _ => anyhow::bail!("unexpected workspace response"),
            }
        }
        Command::List => {
            let workspaces = ui::workspaces(&paths).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&workspaces)?);
            } else {
                for w in workspaces {
                    println!(
                        "{}  {}  {}  {}",
                        w.name,
                        w.state,
                        w.branch,
                        w.path.display()
                    );
                }
            }
        }
        Command::Inspect { workspace } => {
            let workspace = ui::workspace(&paths, workspace, false, cli.json).await?;
            match client::call(&paths, Method::Inspect { workspace }).await? {
                Body::Inspection(inspection) => {
                    if cli.json {
                        println!("{}", serde_json::to_string(&inspection)?);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&inspection)?);
                    }
                }
                _ => anyhow::bail!("unexpected inspection response"),
            }
        }
        Command::Stop { workspace } => {
            let workspace = ui::workspace(&paths, workspace, false, cli.json).await?;
            client::call(&paths, Method::Stop { workspace }).await?;
            output(
                cli.json,
                "Workspace processes stopped",
                json!({"stopped": true}),
            );
        }
        Command::Rm {
            workspace,
            yes,
            keep_branch,
            delete_branch,
        } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let caller_pid = std::process::id();
            let check = match client::call(
                &paths,
                Method::CheckRemoval {
                    workspace: workspace.clone(),
                    caller_pid,
                },
            )
            .await?
            {
                Body::RemovalCheck(check) => check,
                _ => anyhow::bail!("unexpected removal check response"),
            };
            let choice = if keep_branch {
                removal::Choice::KeepBranch
            } else if delete_branch {
                removal::Choice::DeleteBranch
            } else if !check.needs_choice() {
                removal::Choice::Auto
            } else {
                ensure!(
                    !yes,
                    "choose --keep-branch or --delete-branch with --yes for a dirty or differing workspace"
                );
                ui::choose_removal(&check, cli.json)?
            };
            let inspection = match client::call(
                &paths,
                Method::Inspect {
                    workspace: workspace.clone(),
                },
            )
            .await?
            {
                Body::Inspection(inspection) => inspection,
                _ => anyhow::bail!("unexpected inspection response"),
            };
            let cwd = std::env::current_dir()?;
            let inside = std::fs::canonicalize(&inspection.workspace.path)
                .is_ok_and(|root| cwd.starts_with(root));
            let destination = if inside {
                ui::repositories(&paths)
                    .await?
                    .into_iter()
                    .find(|r| r.id == inspection.workspace.repository_id)
                    .map(|r| r.path)
            } else {
                None
            };
            let result = client::call(
                &paths,
                Method::Remove {
                    workspace,
                    choice,
                    caller_pid,
                },
            )
            .await;
            if inside && (result.is_ok() || !cwd.exists()) {
                let destination = destination
                    .filter(|p| p.is_dir())
                    .unwrap_or_else(|| paths.home.clone());
                shell::navigate(&destination, cli.json)?;
            }
            let result = match result? {
                Body::RemovalResult(result) => result,
                _ => anyhow::bail!("unexpected removal response"),
            };
            let message = match (&result.branch, result.branch_deleted) {
                (Some(branch), true) => format!("Workspace and Git branch {branch} removed"),
                (Some(branch), false) => format!(
                    "Workspace removed; Git branch {branch} retained ({})",
                    result.branch_outcome
                ),
                (None, _) => "Workspace removed".into(),
            };
            output(cli.json, &message, serde_json::to_value(&result)?);
        }
        Command::Exec { workspace, command } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            return execution::run(&paths, workspace, command).await;
        }
        Command::Claude { workspace, args } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let Body::Inspection(inspection) =
                client::call(&paths, Method::Inspect { workspace }).await?
            else {
                anyhow::bail!("unexpected workspace response");
            };
            let command = std::iter::once("claude".into())
                .chain(args)
                .chain(["--remote-control".into(), inspection.workspace.name.into()])
                .collect();
            return execution::run(&paths, inspection.workspace.id, command).await;
        }
        Command::Codex { workspace, args } => {
            let workspace = ui::workspace(&paths, workspace, true, cli.json).await?;
            let command = std::iter::once("codex".into())
                .chain(args)
                .chain([
                    "--sandbox".into(),
                    "workspace-write".into(),
                    "--ask-for-approval=never".into(),
                ])
                .collect();
            return execution::run(&paths, workspace, command).await;
        }
        Command::Setup {
            dry_run,
            executable,
        } => {
            let executable = service::executable(executable)?;
            if dry_run {
                let definition =
                    service::definition(&paths, &executable, service::Platform::current()?)?;
                if cli.json {
                    println!("{}", serde_json::to_string(&definition)?);
                } else {
                    println!("# {}\n{}", definition.path.display(), definition.content);
                }
                return Ok(0);
            }
            if let Some(status) = client::status(&paths).await? {
                ensure!(
                    status.managed,
                    "a foreground daemon is running; stop it before setting up the service"
                );
            }
            service::setup(&paths, &executable).await?;
            client::wait(&paths, true).await?;
            output(
                cli.json,
                "Daemon service installed and running",
                json!({"running": true, "service_file": service::file(&paths, service::Platform::current()?), "shell_init": shell::INIT_COMMAND}),
            );
            if !cli.json {
                println!(
                    "\nAdd this line to ~/.zshrc or ~/.bashrc to navigate after add/rm:\n\n{}",
                    shell::INIT_COMMAND
                );
            }
        }
        Command::Daemon {
            command: DaemonCommand::Run { managed },
        } => daemon::run(paths, managed).await?,
        Command::Daemon {
            command: DaemonCommand::Status,
        } => {
            let status = client::status(&paths).await?;
            let running = status.is_some();
            let message = status
                .as_ref()
                .map(|s| format!("Daemon running (PID {}, version {})", s.pid, s.version))
                .unwrap_or_else(|| "Daemon is not running".into());
            output(
                cli.json,
                &message,
                json!({"running": running, "daemon": status, "socket": paths.socket}),
            );
            return Ok(if running { 0 } else { 1 });
        }
        Command::Daemon {
            command: DaemonCommand::Start,
        } => {
            service::start(&paths).await?;
            client::wait(&paths, true).await?;
            output(cli.json, "Daemon started", json!({"running": true}));
        }
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => {
            stop(&paths).await?;
            output(cli.json, "Daemon stopped", json!({"running": false}));
        }
        Command::Daemon {
            command: DaemonCommand::Restart,
        } => {
            // Service administration must still work after a protocol upgrade.
            if let Ok(Some(status)) = client::status(&paths).await {
                ensure!(
                    status.managed,
                    "foreground daemon: stop it and run `shoal daemon run` again"
                );
            }
            stop(&paths).await?;
            service::start(&paths).await?;
            client::wait(&paths, true).await?;
            output(cli.json, "Daemon restarted", json!({"running": true}));
        }
    }
    Ok(0)
}

async fn enter_workspace(paths: &Paths, workspace: String, json_output: bool) -> Result<()> {
    let Body::Inspection(inspection) = client::call(paths, Method::Inspect { workspace }).await?
    else {
        anyhow::bail!("unexpected inspection response");
    };
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    output(
        json_output,
        &inspection.workspace.path.display().to_string(),
        json!({"path": inspection.workspace.path}),
    );
    shell::navigate(&inspection.workspace.path, json_output)
}

async fn stop(paths: &Paths) -> Result<()> {
    match client::status(paths).await {
        Ok(Some(status)) if !status.managed => {
            client::call(paths, protocol::Method::Shutdown).await?;
        }
        Err(error) if !service::file(paths, service::Platform::current()?).exists() => {
            return Err(error);
        }
        _ => service::stop(paths).await?,
    }
    client::wait(paths, false).await
}

fn output(json_output: bool, message: &str, value: serde_json::Value) {
    if json_output {
        println!("{value}");
    } else {
        println!("{message}");
    }
}

async fn sim_command(paths: &Paths, command: cli::SimCommand, json_output: bool) -> Result<i32> {
    use cli::SimCommand;
    match command {
        SimCommand::Catalog => {
            let Body::SimCatalog(catalog) = client::call(paths, Method::SimCatalog).await? else {
                anyhow::bail!("unexpected simulator catalog response");
            };
            if json_output {
                println!("{catalog}");
            } else {
                println!("{}", serde_json::to_string_pretty(&catalog)?);
            }
        }
        SimCommand::List { workspace, all } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let Body::Simulators(sims) = client::call(paths, Method::SimList { workspace }).await?
            else {
                anyhow::bail!("unexpected simulator list response");
            };
            if json_output {
                println!("{}", serde_json::to_string(&sims)?);
            } else {
                for sim in &sims {
                    println!(
                        "{}  {}  {}  {}  {}",
                        sim.udid.as_deref().unwrap_or("pending"),
                        sim.lease_name.as_deref().unwrap_or("idle"),
                        sim.state,
                        sim.device,
                        sim.runtime
                    );
                }
                if sims.is_empty() {
                    println!("No managed simulators");
                }
            }
        }
        SimCommand::Acquire {
            workspace,
            name,
            profile,
            device,
            runtime,
            reason,
            clean,
            wait,
        } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            let request = simulators::SimRequest {
                request_id: uuid::Uuid::new_v4().to_string(),
                clean,
                name,
                profile,
                device,
                runtime,
                reason,
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
            loop {
                match client::call(
                    paths,
                    Method::SimAcquire {
                        workspace: workspace.clone(),
                        request: request.clone(),
                    },
                )
                .await?
                {
                    Body::Simulator(sim) => {
                        output(
                            json_output,
                            &format!(
                                "{}={} ({}, {})",
                                sim.lease_name.as_deref().unwrap_or("default"),
                                sim.udid.as_deref().unwrap_or("pending"),
                                sim.device,
                                sim.runtime
                            ),
                            serde_json::to_value(&sim)?,
                        );
                        break;
                    }
                    Body::SimBusy { message } if tokio::time::Instant::now() >= deadline => {
                        output(
                            json_output,
                            &message,
                            json!({"acquired":false,"code":"simulator_busy","message":message}),
                        );
                        return Ok(2);
                    }
                    Body::SimBusy { .. } => {
                        tokio::time::sleep(
                            std::time::Duration::from_secs(1).min(
                                deadline.saturating_duration_since(tokio::time::Instant::now()),
                            ),
                        )
                        .await
                    }
                    _ => anyhow::bail!("unexpected simulator acquisition response"),
                }
            }
        }
        SimCommand::History {
            workspace,
            all,
            limit,
            before,
        } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let Body::SimHistory(entries) = client::call(
                paths,
                Method::SimHistory {
                    workspace,
                    limit,
                    before,
                },
            )
            .await?
            else {
                anyhow::bail!("unexpected simulator history response");
            };
            if json_output {
                println!("{}", serde_json::to_string(&entries)?);
            } else {
                for entry in &entries {
                    let r = &entry.request;
                    println!(
                        "#{} at {}  {}/{}  {}  action={}  erased-apps={}  actor={}\n  reason: {}{}",
                        entry.id,
                        r.requested_at,
                        r.workspace_name,
                        r.request.name,
                        r.status,
                        r.action.as_deref().unwrap_or("none"),
                        r.apps_removed
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown/not erased".into()),
                        r.execution_id.as_deref().unwrap_or("unscoped caller"),
                        r.request.reason.as_deref().unwrap_or("MISSING"),
                        r.error
                            .as_ref()
                            .map(|e| format!("\n  {e}"))
                            .unwrap_or_default()
                    );
                    for evicted in &r.evicted {
                        println!(
                            "  eviction planned: {} ({} apps)",
                            evicted.udid.as_deref().unwrap_or(&evicted.id),
                            evicted
                                .installed_apps
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "unknown".into())
                        );
                    }
                }
                if entries.is_empty() {
                    println!("No clean-device requests");
                }
            }
        }
        SimCommand::Release { name, workspace } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            client::call(paths, Method::SimRelease { workspace, name }).await?;
            output(json_output, "Simulator released", json!({"released":true}));
        }
    }
    Ok(0)
}

async fn resource_command(
    paths: &Paths,
    command: cli::ResourceCommand,
    json_output: bool,
) -> Result<i32> {
    use cli::ResourceCommand;
    match command {
        ResourceCommand::Acquire {
            mode,
            pool,
            workspace,
            resource,
            name,
            reason,
            wait,
        } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            let request = resources::AcquireRequest {
                mode,
                pool,
                resource,
                name,
                reason,
            };
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait);
            loop {
                match client::call(
                    paths,
                    Method::ResourceAcquire {
                        workspace: workspace.clone(),
                        request: request.clone(),
                    },
                )
                .await?
                {
                    Body::ResourceLease(lease) => {
                        output(
                            json_output,
                            &format!(
                                "{}/{} -> {} [{}] ({})",
                                lease.pool, lease.name, lease.resource, lease.mode, lease.id
                            ),
                            serde_json::to_value(&lease)?,
                        );
                        return Ok(0);
                    }
                    Body::ResourceBusy { message } if tokio::time::Instant::now() >= deadline => {
                        output(
                            json_output,
                            &message,
                            json!({"acquired":false,"code":"resource_busy","pool":request.pool,"resource":request.resource,"message":message}),
                        );
                        return Ok(2);
                    }
                    Body::ResourceBusy { .. } => {
                        tokio::time::sleep(
                            std::time::Duration::from_secs(1).min(
                                deadline.saturating_duration_since(tokio::time::Instant::now()),
                            ),
                        )
                        .await
                    }
                    _ => anyhow::bail!("unexpected resource acquisition response"),
                }
            }
        }
        ResourceCommand::Release {
            pool,
            workspace,
            name,
        } => {
            let workspace = ui::workspace(paths, workspace, true, json_output).await?;
            client::call(
                paths,
                Method::ResourceRelease {
                    workspace,
                    pool,
                    name,
                },
            )
            .await?;
            output(json_output, "Resource released", json!({"released":true}));
        }
        ResourceCommand::List { workspace, all } => {
            let workspace = if all {
                None
            } else {
                Some(ui::workspace(paths, workspace, true, json_output).await?)
            };
            let Body::ResourceLeases(leases) =
                client::call(paths, Method::ResourceList { workspace }).await?
            else {
                anyhow::bail!("unexpected resource list response");
            };
            if json_output {
                println!("{}", serde_json::to_string(&leases)?);
            } else {
                for lease in &leases {
                    println!(
                        "{}  {}/{} -> {} [{}]{}",
                        lease.workspace_id,
                        lease.pool,
                        lease.name,
                        lease.resource,
                        lease.mode,
                        lease
                            .reason
                            .as_ref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    );
                }
                if leases.is_empty() {
                    println!("No resource leases");
                }
            }
        }
    }
    Ok(0)
}
