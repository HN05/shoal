//! `shoal restore --from`: continue spaces from the packages `shoal move`
//! wrote: record each space as removed here and open it again as
//! `shoal restore` does, then the spaces that go with it.
use super::reopen::ReopenedExt;
use crate::transfer::ManifestExt;
use crate::transfer::PackagedSpaceExt;
use shoal_protocol::transcripts::CarriedSessionFile;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, IsTerminal},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::client::output::{
    restore::Reopened,
    transfer::{ImportFailure, Imports},
};
use crate::{
    cli::{
        commands::transfer::STANDARD_STREAM,
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::RestoredSpace,
    refusal::{Reason, Refusal},
    transfer::{ImportedSessionFile, Manifest, PackagedSpace, Parts, Sessions, SpaceImport},
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::space_ids::SpaceId;

use super::reopen;

mod carried;

use carried::CarriedImport;

/// How `shoal restore --from` opens each space.
pub(in crate::cli::commands) struct ImportOptions {
    /// The repository to import into instead of the one with the export's
    /// origin.
    pub(in crate::cli::commands) repository: Option<String>,
    /// Whether the agents keep their sessions; `None` keeps those on the
    /// machine that exported the space or whose files it carries.
    pub(in crate::cli::commands) sessions: Option<bool>,
    /// Whether to queue the restores of the agents the export stopped.
    pub(in crate::cli::commands) restore: bool,
}

/// `shoal restore --from`: continue the spaces exported to `files`, or to
/// the `.shoal` files in the directories among them, as `options` says. A
/// directory or several files import each space and report every result.
pub(in crate::cli::commands) async fn import(
    ctx: &Context,
    files: Vec<PathBuf>,
    options: ImportOptions,
) -> Result<i32> {
    if let [file] = files.as_slice()
        && !file.is_dir()
    {
        // Nothing is recorded yet, so a program that moves the space knows
        // it is not here.
        let package = Package::read(file, &options)
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        let restored = package.open(ctx).await?;
        let opened = restored.id().clone();
        let reopened = reopen::finish_reopen(ctx, restored, "Imported", options.restore).await;
        let failed = carried::open(ctx, &package, &opened, options.restore).await;
        let code = reopened?;
        carried::ensure_opened(failed)?;
        return Ok(code);
    }
    let files = import_files(files)?;
    import_each(ctx, &files, &options).await.report(ctx)
}

/// A package read for its import: the space, the repository it goes to,
/// its agents as they continue here, the spaces that go with it, the
/// session files they keep and the bundles of the workspaces' checkpoints.
struct Package {
    space: PackagedSpace,
    /// Set for a copy, which opens as new spaces.
    copy: bool,
    /// None for a chat without a repository.
    repository: Option<crate::repository::Selector>,
    sessions: Sessions,
    carried: Vec<CarriedImport>,
    files: Vec<ImportedSessionFile>,
    bundles: BTreeMap<SpaceId, tempfile::NamedTempFile>,
    /// Where the session files and bundles are, while the package lives.
    _directory: tempfile::TempDir,
}

impl Package {
    /// Read the package in `file` for the repository with its origin, or
    /// the one `options` names, unless it holds a chat without a
    /// repository, keeping the sessions `options` says.
    fn read(file: &Path, options: &ImportOptions) -> Result<Self> {
        let (manifest, parts, directory) = read_import(file)?;
        let Parts {
            sessions: files,
            bundles,
        } = parts;
        if let Some(copied) = manifest.copy {
            ensure!(
                options.sessions != Some(true),
                "{} holds a copy, whose agents start from forks of the original's sessions; --keep-sessions would continue those sessions",
                file.display()
            );
            if copied.earlier_files {
                eprintln!(
                    "warning: the agents' conversations in {} go past the copied checkpoint and describe work its files do not contain",
                    file.display()
                );
            }
        }
        let repository = match manifest.space.record.repository_id {
            Some(_) => Some(destination(&manifest, options)?),
            None => None,
        };
        let sessions = sessions_here(&manifest, options.sessions);
        let carried = carried::carried_here(&manifest, repository.as_ref(), options.sessions)?;
        let kept: BTreeSet<&AgentSessionId> = (sessions.reported())
            .map(|(_, session)| session)
            .chain(carried.iter().flat_map(CarriedImport::sessions))
            .collect();
        let files = files
            .into_iter()
            .filter(|imported| kept.contains(&imported.file.session))
            .collect();
        Ok(Self {
            space: manifest.space,
            copy: manifest.copy.is_some(),
            repository,
            sessions,
            carried,
            files,
            bundles,
            _directory: directory,
        })
    }

    /// Record the packaged space as removed in its repository, or in none,
    /// keeping its agents' session files with the record for the restore
    /// that opens it, and open it again, once the spaces it carries can
    /// follow it.
    async fn open(&self, ctx: &Context) -> Result<RestoredSpace> {
        let own: Vec<&AgentSessionId> = (self.sessions.reported())
            .map(|(_, session)| session)
            .collect();
        let import = SpaceImport {
            repository: self.repository.clone(),
            bundle: self.bundle_of(&self.space.record.space_id),
            prerequisite: self.space.prerequisite.clone(),
            space: self.space.record_with(self.sessions.clone()),
            copy: self.copy,
            session_files: self.carried_for(&own),
            copies: BTreeMap::new(),
            carried: self.carried.iter().map(CarriedImport::target).collect(),
        };
        let importing = crate::client::transfer::import(&ctx.paths, import);
        ctx.progress("Importing", importing).await
    }

    /// The bundle of the checkpoint of the packaged workspace `id`, while
    /// the package lives.
    fn bundle_of(&self, id: &SpaceId) -> Option<PathBuf> {
        (self.bundles.get(id)).map(|bundle| bundle.path().to_owned())
    }

    /// The session files the package carries for `sessions`.
    fn carried_for(&self, sessions: &[&AgentSessionId]) -> Vec<CarriedSessionFile> {
        (self.files.iter())
            .filter(|imported| sessions.contains(&&imported.file.session))
            .map(ImportedSessionFile::carried)
            .collect()
    }
}

/// The export files `files` names, with each directory among them replaced
/// by its `.shoal` files in name order.
fn import_files(files: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for file in files {
        ensure!(
            file.as_os_str() != STANDARD_STREAM,
            "standard input holds one export; restore --from - on its own"
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

/// Import each of `files`, going on after one fails, then the spaces that
/// go with the space of each.
async fn import_each(ctx: &Context, files: &[PathBuf], options: &ImportOptions) -> ImportReport {
    let mut report = ImportReport::default();
    for file in files {
        let package = match Package::read(file, options) {
            Ok(package) => package,
            Err(error) => {
                report.failed(ctx, file, &error);
                continue;
            }
        };
        if let Some(opened) = import_one(ctx, file, &package, options, &mut report).await {
            for error in carried::open(ctx, &package, &opened, options.restore).await {
                report.failed(ctx, file, &error);
            }
        }
    }
    report
}

/// Open the space `package` holds, from `file`, and set a workspace up,
/// queueing its agents' restores once its post-setup hook succeeded when
/// `options` says; the ID it opened under.
async fn import_one(
    ctx: &Context,
    file: &Path,
    package: &Package,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> Option<SpaceId> {
    let restored = match package.open(ctx).await {
        Ok(restored) => restored,
        Err(error) => {
            report.failed(ctx, file, &error);
            return None;
        }
    };
    let opened = restored.id().clone();
    let mut reopened = match reopen::prepare_set_up(ctx, restored).await {
        Ok(reopened) => reopened,
        Err(error) => {
            report.failed(ctx, file, &error);
            return None;
        }
    };
    let post_setup = reopen::run_post_setup(ctx, &reopened).await;
    if post_setup.is_ok() && options.restore {
        reopened.queue_agents(ctx).await;
    }
    let message = reopened.message(ctx, "Imported");
    let name = reopened.space.name.clone();
    report.imported(ctx, message, reopened);
    if let Err(error) = post_setup {
        let error = error.context(format!("imported {name}, but its hooks.post_setup failed"));
        report.failed(ctx, file, &error);
    }
    Some(opened)
}

/// The spaces an import of several files continued, and the files it could
/// not import.
#[derive(Default)]
struct ImportReport {
    imported: Vec<Reopened>,
    failed: Vec<ImportFailure>,
}

impl ImportReport {
    fn imported(&mut self, ctx: &Context, message: String, value: Reopened) {
        if !ctx.json {
            outln!("{message}");
        }
        self.imported.push(value);
    }

    fn failed(&mut self, ctx: &Context, file: &Path, error: &anyhow::Error) {
        eprintln!(
            "{} cannot import {}: {error:#}",
            Palette::stderr(ctx.json).paint(Style::Error, "error:"),
            file.display()
        );
        self.failed.push(ImportFailure {
            file: file.to_owned(),
            error: format!("{error:#}"),
        });
    }

    fn report(self, ctx: &Context) -> Result<i32> {
        let imported = self.imported.len();
        let message = if self.failed.is_empty() {
            format!("Imported {imported} spaces")
        } else {
            format!("Imported {imported} spaces; {} failed", self.failed.len())
        };
        let code = if self.failed.is_empty() { 0 } else { 1 };
        ctx.emit(
            &message,
            Imports {
                imported: self.imported,
                failed: self.failed,
            },
        )?;
        Ok(code)
    }
}

/// The repository a package of a space in one goes to: the one `options`
/// names, else the one with the exported repository's origin.
fn destination(
    manifest: &Manifest,
    options: &ImportOptions,
) -> Result<crate::repository::Selector> {
    let repository = options
        .repository
        .clone()
        .or_else(|| manifest.remote_url.clone())
        .context("the exported repository has no origin; select the repository to import into with --repo")?;
    ui::repository_selector(repository)
}

/// The package's agents to import here, which keep their sessions as
/// `sessions` says, or on the machine that exported them.
fn sessions_here(manifest: &Manifest, sessions: Option<bool>) -> Sessions {
    manifest.sessions_on(crate::process::machine::id().as_deref(), sessions)
}

/// Read the export in `file`, or on standard input for `-`, which must
/// not be a terminal, since no one types an archive, with its parts in a
/// new temporary directory.
fn read_import(file: &Path) -> Result<(Manifest, Parts, tempfile::TempDir)> {
    let mut input: Box<dyn BufRead> = if file.as_os_str() == STANDARD_STREAM {
        ensure!(
            !std::io::stdin().is_terminal(),
            "--from - reads a package piped in; name its file instead"
        );
        Box::new(std::io::stdin().lock())
    } else {
        let input =
            std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
        Box::new(BufReader::new(input))
    };
    let directory = tempfile::tempdir()?;
    let manifest = crate::transfer::read_manifest(&mut input)?;
    let parts = Parts::read(&mut input, &manifest, directory.path())?;
    Ok((manifest, parts, directory))
}

#[cfg(test)]
mod tests;
