//! `shoal move import`: open workspaces again from the files `shoal move
//! export` wrote.
use std::{
    collections::BTreeSet,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        agents::{self, SessionHome},
        client::request,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::{RemovedWorkspace, RestoredWorkspace},
    protocol::RemovalMethod,
    transfer::{Import, Manifest},
};

use super::{super::restore::Reopened, STANDARD_STREAM};

/// How `shoal move import` opens each workspace.
pub(super) struct ImportOptions {
    /// The repository to import into instead of the one with the export's
    /// origin.
    pub repository: Option<String>,
    /// Whether the stopped agents keep their sessions; `None` keeps those
    /// on the machine that exported the workspace or whose files it carries.
    pub sessions: Option<bool>,
    /// Whether to queue the restores of the agents the export stopped.
    pub resume: bool,
}

/// `shoal move import`: open the workspaces exported to `files`, or to the
/// `.shoal` files in the directories among them, as `options` says. A
/// directory or several files import each workspace and report every result.
pub(super) async fn import(
    ctx: &Context,
    files: Vec<PathBuf>,
    options: ImportOptions,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot import workspaces; ask the user to run shoal move import"
    );
    if let [file] = files.as_slice()
        && !file.is_dir()
    {
        let restored = import_workspace(ctx, file, &options).await?;
        return super::super::restore::finish_reopen(ctx, restored, "Imported", options.resume)
            .await;
    }
    let files = import_files(files)?;
    import_each(ctx, &files, &options).await.report(ctx)
}

/// Record the workspace exported to `file` as removed in its repository, or
/// in the one `options` names, and open it again there.
async fn import_workspace(
    ctx: &Context,
    file: &Path,
    options: &ImportOptions,
) -> Result<RestoredWorkspace> {
    let import = read_import(file)?;
    let repository = options
        .repository
        .clone()
        .or_else(|| import.manifest.remote_url.clone())
        .context("the exported repository has no origin; select the repository to import into with --repo")?;
    let selector = ui::repository_selector(repository)?;
    let record = workspace_here(&import.manifest, options.sessions);
    let kept = kept_sessions(&record);
    let carried: Vec<_> = import
        .sessions
        .into_iter()
        .filter(|imported| kept.contains(&imported.file.session))
        .collect();
    let method = RemovalMethod::ImportWorkspace {
        repository: selector.clone(),
        bundle: import
            .bundle
            .as_ref()
            .map(|bundle| bundle.path().to_owned()),
        prerequisite: import.manifest.prerequisite.clone(),
        workspace: Box::new(record),
    };
    let restored = ctx
        .progress(
            "Importing workspace",
            request::<RestoredWorkspace>(&ctx.paths, method),
        )
        .await;
    // A workspace recorded but not opened keeps its sessions for `shoal
    // work restore`.
    let home = match &restored {
        Ok(restored) => SessionHome::Workspace(&restored.opened.workspace),
        Err(_) => SessionHome::Repository(selector),
    };
    agents::install_session_files(ctx, home, &carried).await;
    restored
}

/// The sessions the agents of the imported record `removed` continue.
fn kept_sessions(removed: &RemovedWorkspace) -> BTreeSet<String> {
    let stopped = removed
        .agents
        .iter()
        .filter_map(|agent| agent.record.session.clone());
    let last = removed
        .last_sessions
        .iter()
        .filter_map(|last| last.session.clone());
    stopped.chain(last).collect()
}

/// The export files `files` names, with each directory among them replaced
/// by its `.shoal` files in name order.
fn import_files(files: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for file in files {
        ensure!(
            file.as_os_str() != STANDARD_STREAM,
            "standard input holds one export; import - on its own"
        );
        if !file.is_dir() {
            found.push(file);
            continue;
        }
        let mut exports = Vec::new();
        for entry in std::fs::read_dir(&file).with_context(|| format!("read {}", file.display()))? {
            let path = entry?.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "shoal")
            {
                exports.push(path);
            }
        }
        ensure!(
            !exports.is_empty(),
            "{} holds no .shoal files",
            file.display()
        );
        exports.sort();
        found.extend(exports);
    }
    Ok(found)
}

/// Import each of `files`, going on after one fails.
async fn import_each(ctx: &Context, files: &[PathBuf], options: &ImportOptions) -> ImportReport {
    let mut report = ImportReport::default();
    for file in files {
        match import_and_prepare(ctx, file, options).await {
            Ok(mut reopened) => {
                let workspace = reopened.workspace.clone();
                let post_setup = super::super::workspaces::run_post_setup(ctx, &workspace).await;
                // The agents start in a workspace whose post-setup hook succeeded.
                if post_setup.is_ok() && options.resume {
                    reopened.queue_agents(ctx).await;
                }
                report.imported(ctx, reopened);
                if let Err(error) = post_setup {
                    let error = error.context(format!(
                        "imported {}, but its hooks.post_setup failed",
                        workspace.name
                    ));
                    report.failed(ctx, file, &error);
                }
            }
            Err(error) => report.failed(ctx, file, &error),
        }
    }
    report
}

/// Import the workspace exported to `file` and set it up.
async fn import_and_prepare(
    ctx: &Context,
    file: &Path,
    options: &ImportOptions,
) -> Result<Reopened> {
    let restored = import_workspace(ctx, file, options).await?;
    super::super::restore::prepare_reopened(ctx, restored)
        .await?
        .context("its setup did not finish")
}

/// The workspaces an import of several files opened, and the files it could
/// not import.
#[derive(Default)]
struct ImportReport {
    imported: Vec<Reopened>,
    failed: Vec<serde_json::Value>,
}

impl ImportReport {
    fn imported(&mut self, ctx: &Context, reopened: Reopened) {
        if !ctx.json {
            outln!("{}", reopened.message(ctx, "Imported"));
        }
        self.imported.push(reopened);
    }

    fn failed(&mut self, ctx: &Context, file: &Path, error: &anyhow::Error) {
        eprintln!(
            "{} cannot import {}: {error:#}",
            Palette::stderr(ctx.json).paint(Style::Error, "error:"),
            file.display()
        );
        self.failed
            .push(json!({"file": file, "error": format!("{error:#}")}));
    }

    fn report(self, ctx: &Context) -> Result<i32> {
        let imported = self.imported.len();
        let message = if self.failed.is_empty() {
            format!("Imported {imported} workspaces")
        } else {
            format!(
                "Imported {imported} workspaces; {} failed",
                self.failed.len()
            )
        };
        ctx.emit(
            &message,
            json!({"imported": self.imported, "failed": self.failed}),
        )?;
        Ok(if self.failed.is_empty() { 0 } else { 1 })
    }
}

/// The exported removal record to import here, whose agents keep their
/// sessions as `sessions` says, or on the machine that exported it.
fn workspace_here(manifest: &Manifest, sessions: Option<bool>) -> RemovedWorkspace {
    manifest.workspace_on(crate::process::machine::id().as_deref(), sessions)
}

/// Read the export in `file`, or on standard input for `-`.
fn read_import(file: &Path) -> Result<Import> {
    if file.as_os_str() == STANDARD_STREAM {
        return Import::read(&mut std::io::stdin().lock());
    }
    let input = std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    Import::read(&mut BufReader::new(input))
}
