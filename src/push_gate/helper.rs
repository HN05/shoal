//! `git-remote-shoal`, the remote helper Git runs for an agent's push to a
//! rewritten URL. It speaks Git's remote helper protocol on standard input
//! and output: it lists the remote's refs, takes the push options and the
//! refs to push, asks the daemon about each push, pushes those it allows to
//! the real URL with Git, and reports the others as waiting for approval.
//! Standard output carries only the protocol.
use std::collections::BTreeMap;

use std::io::{BufRead as _, Write as _};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    client::links::PushOutcome,
    daemon::approvals::PushSpecification,
    git,
    protocol::ApprovalMethod,
    tools::Tool,
};

/// Answer Git for a push to `remote`, by name or URL, whose real URL is
/// `url`. Git waits for each reply before it writes again, so reading and
/// writing block.
pub async fn run(ctx: &Context, remote: String, url: String) -> Result<i32> {
    let mut lines = std::io::stdin().lock().lines();
    let mut stdout = std::io::stdout().lock();
    let mut session = Session {
        remote,
        url,
        listed: BTreeMap::new(),
        leases: BTreeMap::new(),
        pushes: Vec::new(),
    };
    while let Some(line) = lines.next().transpose()? {
        let Some(reply) = session.answer(ctx, &line).await? else {
            return Ok(0);
        };
        stdout.write_all(reply.as_bytes())?;
        stdout.flush()?;
    }
    Ok(0)
}

/// What Git has told the helper so far.
struct Session {
    remote: String,
    url: String,
    /// The remote's refs and their objects, as listed for the push.
    listed: BTreeMap<String, String>,
    /// `--force-with-lease` expectations: ref → expected object, empty when
    /// the ref must not exist.
    leases: BTreeMap<String, String>,
    /// The refspecs of the push batch Git is sending.
    pushes: Vec<String>,
}

impl Session {
    /// The reply to one line from Git, or `None` once Git is done.
    async fn answer(&mut self, ctx: &Context, line: &str) -> Result<Option<String>> {
        Ok(Some(match line {
            "capabilities" => "push\noption\n\n".to_owned(),
            "list" | "list for-push" => self.list().await?,
            "" if self.pushes.is_empty() => return Ok(None),
            "" => self.push(ctx).await,
            _ => {
                if let Some(option) = line.strip_prefix("option ") {
                    self.option(option).to_owned()
                } else if let Some(refspec) = line.strip_prefix("push ") {
                    self.pushes.push(refspec.to_owned());
                    String::new()
                } else {
                    bail!("unexpected request from Git: {line}")
                }
            }
        }))
    }

    /// The remote's refs, as Git's `list` command expects them.
    async fn list(&mut self) -> Result<String> {
        let listed = git_output(&git::list_remote_args(&self.url)).await?;
        let mut reply = String::new();
        for line in listed.lines() {
            let Some((object, name)) = line.split_once('\t') else {
                continue;
            };
            if name.ends_with("^{}") {
                continue;
            }
            reply.push_str(&format!("{object} {name}\n"));
            self.listed.insert(name.to_owned(), object.to_owned());
        }
        reply.push('\n');
        Ok(reply)
    }

    /// Accept the options a push passes on; others, such as a dry run, are
    /// refused, and Git stops before pushing.
    fn option(&mut self, option: &str) -> &'static str {
        let (name, value) = option.split_once(' ').unwrap_or((option, ""));
        match name {
            "verbosity" | "progress" | "force" => "ok\n",
            "cas" => {
                let (name, expected) = value.split_once(':').unwrap_or((value, ""));
                self.leases.insert(name.to_owned(), expected.to_owned());
                "ok\n"
            }
            _ => "unsupported\n",
        }
    }

    /// Push or hold back each ref of the batch and report each one's status.
    async fn push(&mut self, ctx: &Context) -> String {
        let refspecs = std::mem::take(&mut self.pushes);
        let mut direct = Vec::new();
        let mut reply = String::new();
        let workspace = workspace(ctx).await;
        for refspec in refspecs {
            let destination = destination(&refspec).to_owned();
            let outcome = match &workspace {
                Ok(workspace) => self.request(ctx, workspace, &refspec).await,
                Err(error) => Err(anyhow::anyhow!("{error:#}")),
            };
            match outcome {
                Ok(PushOutcome::Direct) => direct.push(refspec),
                Ok(PushOutcome::Proposed(proposed)) => {
                    let id = proposed.proposed.id;
                    eprintln!(
                        "Push of {destination} proposed for approval {id}: the user decides whether Shoal pushes it, and the decision arrives as a message."
                    );
                    reply.push_str(&format!("error {destination} waits for approval {id}\n"));
                }
                Err(error) => reply.push_str(&format!(
                    "error {destination} {}\n",
                    one_line(&format!("{error:#}"))
                )),
            }
        }
        if !direct.is_empty() {
            reply.push_str(&self.push_directly(&direct).await);
        }
        reply.push('\n');
        reply
    }

    /// Ask the daemon whether the push `refspec` names may go ahead from
    /// `workspace`.
    async fn request(&self, ctx: &Context, workspace: &str, refspec: &str) -> Result<PushOutcome> {
        let push = self.specification(refspec).await?;
        let workspace = workspace.to_owned();
        client::request(&ctx.paths, ApprovalMethod::RequestPush { workspace, push }).await
    }

    /// What pushing `refspec` changes on the remote: the object its source
    /// names, and for a forced push or a deletion the remote commit it
    /// replaces.
    async fn specification(&self, refspec: &str) -> Result<PushSpecification> {
        let forced = refspec.starts_with('+');
        let (source, destination) = refspec
            .trim_start_matches('+')
            .split_once(':')
            .with_context(|| format!("Git sent a push without a destination: {refspec}"))?;
        let commit = match source {
            "" => None,
            source => Some(
                git_output(&git::resolve_args(source))
                    .await?
                    .trim()
                    .to_owned(),
            ),
        };
        // A forced push or deletion replaces only what the remote had; an
        // empty expectation keeps a missing ref missing.
        let replaces = match self.leases.get(destination) {
            Some(expected) => Some(expected.clone()),
            None if forced || commit.is_none() => {
                Some(self.listed.get(destination).cloned().unwrap_or_default())
            }
            None => None,
        };
        Ok(PushSpecification {
            remote: self.remote.clone(),
            url: self.one_of_several_urls().await,
            destination: destination.to_owned(),
            commit,
            replaces,
            agent: None,
        })
    }

    /// The URL this push goes to, when the remote has several push URLs:
    /// Git runs the helper for each, so approval must push to this one
    /// only. A remote given by URL, or one Git cannot read, has its own.
    async fn one_of_several_urls(&self) -> Option<String> {
        let urls = git_output(&git::push_urls_args(&self.remote)).await.ok()?;
        (urls.lines().count() > 1).then(|| self.url.clone())
    }

    /// Push `refspecs` to the real URL with Git, without the rewrites and
    /// without the hooks Git already ran, and report each ref's status.
    async fn push_directly(&self, refspecs: &[String]) -> String {
        let leases: Vec<_> = refspecs
            .iter()
            .map(|refspec| destination(refspec))
            .filter_map(|destination| {
                let expected = self.leases.get(destination)?;
                Some((destination, expected.as_str()))
            })
            .collect();
        let args = git::porcelain_push_args(&self.url, refspecs, &leases);
        let pushed = git(&args)
            .stderr(std::process::Stdio::inherit())
            .output()
            .await;
        let porcelain = match &pushed {
            Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
            Err(error) => {
                let message = one_line(&format!("git push could not run: {error}"));
                return refspecs
                    .iter()
                    .map(|refspec| format!("error {} {message}\n", destination(refspec)))
                    .collect();
            }
        };
        statuses(refspecs, &porcelain)
    }
}

/// The status line Git expects for each of `refspecs`, from what
/// `git push --porcelain` printed: `ok`, or `error` with Git's reason.
fn statuses(refspecs: &[String], porcelain: &str) -> String {
    let mut reported = BTreeMap::new();
    for line in porcelain.lines() {
        let mut fields = line.split('\t');
        let (Some(flag), Some(refs), summary) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some((_, destination)) = refs.split_once(':') else {
            continue;
        };
        let status = match flag {
            "!" => format!("error {destination} {}", summary.unwrap_or("rejected")),
            _ => format!("ok {destination}"),
        };
        reported.insert(destination.to_owned(), status);
    }
    refspecs
        .iter()
        .map(|refspec| {
            let destination = destination(refspec);
            let status = reported
                .remove(destination)
                .unwrap_or_else(|| format!("error {destination} git push failed"));
            format!("{status}\n")
        })
        .collect()
}

/// The ref a refspec Git sent updates.
fn destination(refspec: &str) -> &str {
    refspec
        .split_once(':')
        .map_or(refspec, |(_, destination)| destination)
}

/// `message` on one line, as a status reason must be.
fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The workspace the push comes from: the one Git runs in, else the
/// caller's own.
async fn workspace(ctx: &Context) -> Result<String> {
    match ui::choose_workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
        Err(error) if error.downcast_ref::<NoCurrentWorkspace>().is_some() => {
            ui::choose_workspace(ctx, None, Fallback::OwnWorkspace).await
        }
        chosen => chosen,
    }
}

/// Git without the rewrites, so it reaches the real remote.
fn git(args: &[String]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(Tool::Git.program());
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped());
    super::strip(&mut command);
    command
}

/// What Git prints for `args`, failing with its error output.
async fn git_output(args: &[String]) -> Result<String> {
    let output = git(args)
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .context("run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().map(String::as_str).unwrap_or_default(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests;
