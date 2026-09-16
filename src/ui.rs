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

fn interactive(json: bool) -> Result<()> {
    ensure!(
        !json && io::stdin().is_terminal() && io::stderr().is_terminal(),
        "missing argument; pass an explicit target/name in non-interactive mode"
    );
    Ok(())
}

pub fn confirm_removal(check: &crate::removal::RemovalCheck, json: bool) -> Result<()> {
    let warnings = check.warnings();
    ensure!(
        !json && io::stdin().is_terminal() && io::stderr().is_terminal(),
        "removal requires confirmation: {}. Pass --yes to confirm",
        warnings.join("; ")
    );
    eprintln!("Remove workspace {}?", check.workspace.name);
    for warning in warnings {
        eprintln!("  - {warning}");
    }
    eprint!("Are you sure? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    ensure!(
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        "removal canceled"
    );
    Ok(())
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
    interactive(json)?;
    ensure!(
        !entries.is_empty(),
        "nothing to select; register a repository with `shoal repo add <path-or-url>` or create a workspace with `shoal add`"
    );
    let mut picker = Command::new("fzf")
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
        .stderr(Stdio::inherit())
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
    let id = String::from_utf8(output.stdout)?
        .split('\t')
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    ensure!(
        entries.iter().any(|entry| entry.0 == id),
        "picker returned an unknown item"
    );
    Ok(id)
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
    let source = repo.source.trim_end_matches('/');
    let name = source.rsplit(['/', ':']).next().unwrap_or(source);
    name.strip_suffix(".git").unwrap_or(name)
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
