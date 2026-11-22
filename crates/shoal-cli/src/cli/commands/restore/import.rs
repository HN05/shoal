//! `shoal restore --from`: continue spaces from the packages `shoal move`
//! wrote: the daemon reads each package, records its spaces as removed here
//! and opens its own space again, which this sets up as `shoal restore`
//! does, then opens the spaces that go with it.
use super::reopen::ReopenedExt;
use std::{
    io::IsTerminal,
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
    transfer::{Claim, ImportedPackage, PackageImport, PackageSource},
};

use super::reopen;
use shoal_workers::reopening;

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
        let imported = import_package(ctx, file, &options).await?;
        let restored = imported.restored;
        let reopened = reopen::finish_reopen(ctx, restored, "Imported", options.restore).await;
        let failed =
            reopening::open_carried(&ctx.paths, ctx, imported.carried, options.restore).await;
        let code = reopened?;
        reopening::ensure_carried_opened(failed)?;
        return Ok(code);
    }
    let files = import_files(files)?;
    import_each(ctx, &files, &options).await.report(ctx)
}

/// Have the daemon import the package in `file`, or on standard input for
/// `-`, as `options` says; an error says when nothing was recorded.
async fn import_package(
    ctx: &Context,
    file: &Path,
    options: &ImportOptions,
) -> Result<ImportedPackage> {
    let import = (package_import(file, options))
        .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
    let importing = async {
        match import.package {
            PackageSource::File(_) => crate::client::transfer::import(&ctx.paths, import).await,
            PackageSource::Stream => {
                let mut input = tokio::io::stdin();
                crate::client::transfer::import_streamed(&ctx.paths, import, &mut input).await
            }
        }
    };
    ctx.progress("Importing", importing).await
}

/// The import of the package in `file`, or on standard input for `-`,
/// which must not be a terminal, since no one types an archive.
fn package_import(file: &Path, options: &ImportOptions) -> Result<PackageImport> {
    let package = if file.as_os_str() == STANDARD_STREAM {
        ensure!(
            !std::io::stdin().is_terminal(),
            "--from - reads a package piped in; name its file instead"
        );
        PackageSource::Stream
    } else {
        let absolute = std::path::absolute(file);
        PackageSource::File(absolute.with_context(|| format!("locate {}", file.display()))?)
    };
    let repository = options.repository.clone().map(ui::repository_selector);
    Ok(PackageImport {
        package,
        repository: repository.transpose()?,
        sessions: options.sessions,
        claim: Claim::Plain,
    })
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
        let imported = match import_package(ctx, file, options).await {
            Ok(imported) => imported,
            Err(error) => {
                report.failed(ctx, file, &error);
                continue;
            }
        };
        let ImportedPackage { restored, carried } = imported;
        if import_one(ctx, file, restored, options, &mut report).await {
            for error in reopening::open_carried(&ctx.paths, ctx, carried, options.restore).await {
                report.failed(ctx, file, &error);
            }
        }
    }
    report
}

/// Set the space the import of `file` opened, `restored`, up, queueing its
/// agents' restores once its post-setup hook succeeded when `options`
/// says; whether it opened.
async fn import_one(
    ctx: &Context,
    file: &Path,
    restored: RestoredSpace,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> bool {
    let mut reopened = match reopening::prepare_set_up(&ctx.paths, ctx, restored).await {
        Ok(reopened) => reopened,
        Err(error) => {
            report.failed(ctx, file, &error);
            return false;
        }
    };
    let post_setup = reopening::run_post_setup(&ctx.paths, ctx, &reopened).await;
    if post_setup.is_ok() && options.restore {
        reopening::queue_agents(&ctx.paths, ctx, &mut reopened).await;
    }
    let message = reopened.message(ctx, "Imported");
    let name = reopened.space.name.clone();
    report.imported(ctx, message, reopened);
    if let Err(error) = post_setup {
        let error = error.context(format!("imported {name}, but its hooks.post_setup failed"));
        report.failed(ctx, file, &error);
    }
    true
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
