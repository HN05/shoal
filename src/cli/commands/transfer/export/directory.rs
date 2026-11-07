//! The directory a batch export writes a file per space to, and the report
//! of what it wrote there.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::client::output::transfer::{Exported, Exports};
use crate::{
    cli::{context::Context, ui},
    transfer::ExportedSpace,
};

/// The directory an export of several spaces goes to, shown as the user
/// named it.
pub(in super::super) struct ExportDirectory {
    path: PathBuf,
    shown: PathBuf,
}

impl ExportDirectory {
    /// Create the directory when it is missing.
    pub(in super::super) fn create(shown: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&shown)
            .with_context(|| format!("create the directory {}", shown.display()))?;
        let path = std::fs::canonicalize(&shown)
            .with_context(|| format!("find the directory {}", shown.display()))?;
        Ok(Self { path, shown })
    }

    /// The directory as the user named it.
    pub(super) fn shown(&self) -> &Path {
        &self.shown
    }

    /// The file the space `name` goes to.
    pub(in super::super) fn file(&self, name: &str) -> PathBuf {
        self.path.join(Self::file_name(name))
    }

    /// The file the space `name` goes to, as the user names it.
    fn shown_file(&self, name: &str) -> PathBuf {
        self.shown.join(Self::file_name(name))
    }

    fn file_name(name: &str) -> String {
        format!("{name}.shoal")
    }
}

/// The report of a batch export into a directory, of the spaces or of
/// copies of them.
pub(super) struct DirectoryReport<'a> {
    directory: &'a ExportDirectory,
    report: ui::Report<Exported>,
    copies: bool,
}

impl<'a> DirectoryReport<'a> {
    pub(super) fn new(directory: &'a ExportDirectory, copies: bool) -> Self {
        let verb = match copies {
            true => "copy",
            false => "export",
        };
        Self {
            directory,
            report: ui::Report::new(verb),
            copies,
        }
    }

    /// Record the export of the space `name` to its file, or its failure,
    /// which does not stop the others.
    pub(super) fn add(&mut self, ctx: &Context, name: &str, exported: Result<ExportedSpace>) {
        let exported = match exported {
            Ok(exported) => exported,
            Err(error) => return self.report.failed(ctx, name, &error),
        };
        let space = match exported.manifest.copy {
            Some(_) => format!("a copy of {name}"),
            None => name.to_owned(),
        };
        let message = format!(
            "Exported {space} to {}",
            self.directory.shown_file(name).display()
        );
        let value = Exported {
            file: self.directory.file(name),
            manifest: exported.manifest,
            removal: exported.removal,
        };
        self.report.done(ctx, &message, value);
    }

    /// Print the summary, or with `--json` every export and failure,
    /// exiting 1 when any failed.
    pub(super) fn finish(self, ctx: &Context) -> Result<i32> {
        let (exported, failed) = self.report.counts();
        let shown = self.directory.shown.display();
        let (spaces, opens) = match self.copies {
            true => ("copies of ", "as new spaces"),
            false => ("", "on another machine"),
        };
        let summary = if failed == 0 {
            format!(
                "Exported {spaces}{exported} spaces to {shown}; shoal move import {shown} opens them {opens}"
            )
        } else {
            format!(
                "Exported {spaces}{exported} of {} spaces to {shown}; {failed} failed",
                exported + failed
            )
        };
        let directory = self.directory.path.clone();
        self.report
            .finish(ctx, &summary, |exported, failed| Exports {
                exported,
                failed,
                directory,
            })
    }
}
