use std::{
    io::{self, IsTerminal, Write},
    process::{Command, Stdio},
};

use crate::{
    client,
    model::{Repository, Workspace},
    paths::Paths,
    protocol::{Body, Method},
};
use anyhow::{Context, Result, bail, ensure};

pub fn is_interactive(json: bool) -> bool {
    !json && io::stdin().is_terminal() && io::stderr().is_terminal()
}

fn interactive(json: bool) -> Result<()> {
    ensure!(
        is_interactive(json),
        "missing argument; pass an explicit target/name in non-interactive mode"
    );
    Ok(())
}

/// Approval stays in the CLI. Piped/JSON callers must opt in explicitly.
pub fn confirm(action: &str, json: bool, flag: &str) -> Result<bool> {
    ensure!(
        is_interactive(json),
        "{action}; confirmation required in non-interactive mode; pass {flag}"
    );
    let mut input = io::stdin().lock();
    let mut output = io::stderr().lock();
    confirm_with_io(action, &mut input, &mut output)
}

fn confirm_with_io(
    action: &str,
    input: &mut impl io::BufRead,
    output: &mut impl Write,
) -> Result<bool> {
    writeln!(output, "{action}")?;
    loop {
        write!(output, "Are you sure? [y/N] ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Ok(false);
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => writeln!(output, "Please enter y or n.")?,
        }
    }
}

pub fn choose_removal(
    check: &crate::removal::RemovalCheck,
    json: bool,
) -> Result<crate::removal::Choice> {
    let warnings = check.warnings();
    ensure!(
        !json && io::stdin().is_terminal() && io::stderr().is_terminal(),
        "removal requires a branch choice: {}. Pass --yes with --keep-branch or --delete-branch",
        warnings.join("; ")
    );
    eprintln!("Workspace: {}", check.workspace.name);
    eprintln!("Branch:    {}", check.branch.as_deref().unwrap_or("none"));
    for warning in warnings {
        eprintln!("  - {warning}");
    }
    let choice = pick(
        "Branch action> ",
        vec![
            ("abort".into(), "Cancel         Keep everything".into()),
            (
                "keep".into(),
                "Keep branch    Delete workspace files only".into(),
            ),
            (
                "delete".into(),
                "Delete branch  Delete workspace files and branch".into(),
            ),
        ],
        json,
    )?;
    match choice.as_str() {
        "keep" => Ok(crate::removal::Choice::KeepBranch),
        "delete" => Ok(crate::removal::Choice::DeleteBranch),
        _ => bail!("removal canceled"),
    }
}

pub fn input(prompt: &str, json: bool) -> Result<String> {
    interactive(json)?;
    eprint!("{prompt}: ");
    io::stderr().flush()?;
    let mut value = String::new();
    io::stdin().read_line(&mut value)?;
    let value = value.trim().to_owned();
    ensure!(!value.is_empty(), "canceled: no value supplied");
    Ok(value)
}

pub fn pick(prompt: &str, entries: Vec<(String, String)>, json: bool) -> Result<String> {
    Ok(pick_with_actions(prompt, entries, json, None)?.1)
}

fn pick_with_actions(
    prompt: &str,
    entries: Vec<(String, String)>,
    json: bool,
    actions: Option<(&str, &str)>,
) -> Result<(String, String)> {
    interactive(json)?;
    ensure!(
        !entries.is_empty(),
        "nothing to select; register a repository with `shoal repo add <path-or-url>` or create a workspace with `shoal add`"
    );
    let mut command = Command::new("fzf");
    command
        .args([
            "--no-sort",
            "--delimiter=\t",
            "--with-nth=2..",
            "--prompt",
            prompt,
        ])
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some((keys, header)) = actions {
        command
            .arg(format!("--expect={keys}"))
            .args(["--header", header]);
    }
    let mut picker = command
        .spawn()
        .context("open picker; install fzf or supply an explicit target")?;
    let mut input = picker.stdin.take().context("picker stdin is unavailable")?;
    for (id, label) in &entries {
        let label = label.replace(['\n', '\r', '\t'], " ");
        // fzf may exit before consuming the entire list when canceled.
        if writeln!(input, "{id}\t{label}").is_err() {
            break;
        }
    }
    drop(input);
    let output = picker.wait_with_output()?;
    ensure!(output.status.success(), "selection canceled");
    let output = String::from_utf8(output.stdout)?;
    let (action, selected) = if actions.is_some() {
        output.split_once('\n').context("picker omitted action")?
    } else {
        ("", output.as_str())
    };
    let id = selected
        .split('\t')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    ensure!(
        entries.iter().any(|entry| entry.0 == id),
        "picker returned an unknown item"
    );
    Ok((action.to_owned(), id))
}

pub async fn workspace_menu(paths: &Paths) -> Result<crate::cli::Command> {
    use crate::cli::Command;
    let repos = repository_choices(repositories(paths).await?).await?;
    let mut entries: Vec<_> = workspaces(paths)
        .await?
        .into_iter()
        .map(|w| {
            let repo = repos
                .iter()
                .find(|(id, _)| id == &w.repository_id)
                .map(|(_, name)| name.as_str())
                .unwrap_or("unknown repository");
            (
                w.id,
                format!("{}  {repo}  {}  {}", w.name, w.state, w.branch),
            )
        })
        .collect();
    let scoped = std::env::var_os("SHOAL_SCOPE_TOKEN").is_some();
    if !scoped {
        entries.push(("add-workspace".into(), "+ Add workspace".into()));
    }
    let (action, id) = pick_with_actions(
        "Shoal> ",
        entries,
        false,
        Some(if scoped {
            (
                "ctrl-e,ctrl-o,ctrl-f",
                "enter: enter   ctrl-e: execute   ctrl-o: inspect   ctrl-f: diff",
            )
        } else {
            (
                "ctrl-d,ctrl-e,ctrl-a,ctrl-o,ctrl-s,ctrl-f",
                "enter: enter   ctrl-d: delete   ctrl-e: execute   ctrl-a: add   ctrl-o: inspect   ctrl-s: stop   ctrl-f: diff",
            )
        }),
    )?;
    if action == "ctrl-a" || (action.is_empty() && id == "add-workspace") {
        return Ok(Command::Add {
            repository: None,
            name: None,
            base: None,
            agent: None,
            args: vec![],
        });
    }
    ensure!(id != "add-workspace", "select a workspace for this action");
    let workspace = Some(id);
    Ok(match action.as_str() {
        "" => Command::Cd { workspace },
        "ctrl-d" => Command::Rm {
            workspace,
            yes: false,
            keep_branch: false,
            delete_branch: false,
        },
        "ctrl-o" => Command::Inspect { workspace },
        "ctrl-s" => Command::Stop { workspace },
        "ctrl-f" => Command::Diff { workspace },
        "ctrl-e" => match pick(
            "Execute> ",
            [
                "claude",
                "codex cli",
                "codex app",
                "t3",
                "custom shell command",
            ]
            .into_iter()
            .map(|s| (s.into(), s.into()))
            .collect(),
            false,
        )?
        .as_str()
        {
            "claude" => Command::Claude {
                workspace,
                args: vec![],
            },
            mode @ ("codex cli" | "codex app") => Command::Codex {
                mode: Some(if mode == "codex cli" {
                    crate::cli::CodexMode::Cli
                } else {
                    crate::cli::CodexMode::App
                }),
                workspace,
                args: vec![],
            },
            "t3" => Command::T3 {
                workspace,
                args: vec![],
            },
            _ => Command::Exec {
                workspace,
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    input("Shell command", false)?.into(),
                ],
            },
        },
        _ => bail!("unknown picker action"),
    })
}

pub async fn repositories(paths: &Paths) -> Result<Vec<Repository>> {
    match client::call(paths, Method::Repositories).await? {
        Body::Repositories(repos) => Ok(repos),
        _ => bail!("unexpected repository response"),
    }
}

pub fn repository_label(repo: &Repository) -> String {
    format!("{}  {}", repository_name(repo), repo.source)
}

fn repository_name(repo: &Repository) -> &str {
    crate::repository::name(repo)
}

pub async fn repository_choices(mut repos: Vec<Repository>) -> Result<Vec<(String, String)>> {
    for repo in &mut repos {
        if let Some(url) = crate::repository::remote_url(&repo.source).await? {
            repo.source = url;
        }
    }
    let labels: Vec<_> = repos
        .iter()
        .map(|repo| {
            let name = repository_name(repo);
            if repos.iter().filter(|r| repository_name(r) == name).count() == 1 {
                return name.to_owned();
            }
            let host = repo
                .source
                .split_once("://")
                .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
                .or_else(|| repo.source.split_once(':').map(|(host, _)| host))
                .filter(|host| !host.is_empty());
            match host {
                Some(host) => format!("{name} ({})", host.rsplit('@').next().unwrap_or(host)),
                None => format!("{name} (local)"),
            }
        })
        .collect();
    Ok(repos
        .into_iter()
        .zip(&labels)
        .map(|(repo, label)| {
            // Same host or multiple local checkouts can still share a name.
            let label = if labels.iter().filter(|other| *other == label).count() > 1 {
                format!("{label}  {}", repo.source)
            } else {
                label.clone()
            };
            (repo.id, label)
        })
        .collect())
}

pub async fn workspaces(paths: &Paths) -> Result<Vec<Workspace>> {
    match client::call(paths, Method::List).await? {
        Body::Workspaces(workspaces) => Ok(workspaces),
        _ => bail!("unexpected workspace response"),
    }
}

pub async fn workspace(
    paths: &Paths,
    explicit: Option<String>,
    current: bool,
    json: bool,
) -> Result<String> {
    if let Some(name) = explicit {
        return Ok(name);
    }
    let workspaces = workspaces(paths).await?;
    if std::env::var_os("SHOAL_SCOPE_TOKEN").is_some() {
        // List is filtered and authorized by the daemon, so this remains bound
        // to the execution even when the process changes its working directory.
        return workspaces
            .first()
            .map(|w| w.id.clone())
            .context("scoped workspace is unavailable");
    }
    if current {
        let cwd = std::fs::canonicalize(std::env::current_dir()?)?;
        if let Some(workspace) = workspaces
            .iter()
            .filter(|w| std::fs::canonicalize(&w.path).is_ok_and(|root| cwd.starts_with(root)))
            .max_by_key(|w| w.path.components().count())
        {
            return Ok(workspace.id.clone());
        }
    }
    pick_workspace(workspaces, json)
}

pub async fn workspace_picker(paths: &Paths, json: bool) -> Result<String> {
    // Always run the picker, including for scoped callers (whose list is filtered).
    let workspaces = workspaces(paths)
        .await?
        .into_iter()
        .filter(|w| w.path.is_dir())
        .collect();
    pick_workspace(workspaces, json)
}

fn pick_workspace(workspaces: Vec<Workspace>, json: bool) -> Result<String> {
    pick(
        "Workspace> ",
        workspaces
            .into_iter()
            .map(|w| {
                (
                    w.id,
                    format!(
                        "{}  {}  {}  {}",
                        w.name,
                        w.state,
                        w.branch,
                        w.path.display()
                    ),
                )
            })
            .collect(),
        json,
    )
}

pub fn repository_selector(value: String) -> Result<String> {
    if std::path::Path::new(&value).exists() {
        Ok(std::fs::canonicalize(&value)?
            .to_str()
            .context("repository path is not UTF-8")?
            .to_owned())
    } else {
        Ok(value)
    }
}
