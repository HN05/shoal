//! `shoal move import`: continue spaces from the packages `shoal move
//! export` wrote: open workspaces again and start chats that resume their
//! sessions.
use std::{
    collections::BTreeSet,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::client::output::chat::BackgroundChat;
use crate::client::output::{
    resume::Reopened,
    transfer::{ImportFailure, Imported, Imports},
};
use crate::{
    cli::{
        agents::{self, SessionHome},
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    model::RestoredWorkspace,
    protocol::RemovalMethod,
    transfer::{
        Import, ImportedSessionFile, Manifest, PackagedChat, PackagedWorkspace, Sessions, Space,
    },
};

use super::{
    super::chat::{self, ChatPlace, Started},
    STANDARD_STREAM,
};

/// How `shoal move import` opens each space.
pub(super) struct ImportOptions {
    /// The repository to import into instead of the one with the export's
    /// origin.
    pub repository: Option<String>,
    /// Whether the agents keep their sessions; `None` keeps those on the
    /// machine that exported the space or whose files it carries.
    pub sessions: Option<bool>,
    /// Whether to queue the restores of the agents the export stopped, and
    /// start chats in the background.
    pub resume: bool,
}

/// `shoal move import`: continue the spaces exported to `files`, or to the
/// `.shoal` files in the directories among them, as `options` says. A
/// directory or several files import each space and report every result.
pub(super) async fn import(
    ctx: &Context,
    files: Vec<PathBuf>,
    options: ImportOptions,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot import spaces; ask the user to run shoal move import"
    );
    if let [file] = files.as_slice()
        && !file.is_dir()
    {
        let package = Package::read(file, &options)?;
        return match package.space {
            Space::Workspace(_) => {
                let restored = package.open_workspace(ctx).await?;
                super::super::restore::finish_reopen(ctx, restored, "Imported", options.resume)
                    .await
            }
            // A chat starts on this terminal, unless it is to start in the
            // background or there is none to start it on.
            Space::Chat(_) => match package.start_chat(ctx, chat_place(ctx, &options)).await? {
                Started::Exited(code) => Ok(code),
                Started::Background(chat) => {
                    ctx.emit(&chat_message(&chat), &chat)?;
                    Ok(0)
                }
            },
        };
    }
    let files = import_files(files)?;
    import_each(ctx, &files, &options).await.report(ctx)
}

/// Where a chat imported on its own starts: beside this terminal, unless
/// `--resume` starts it in the background, as it must without a terminal.
fn chat_place(ctx: &Context, options: &ImportOptions) -> ChatPlace {
    match options.resume || !ctx.interactive() {
        true => ChatPlace::Background,
        false => ChatPlace::Beside,
    }
}

/// The report of an imported chat started in the background.
fn chat_message(chat: &BackgroundChat) -> String {
    format!("Imported chat {}. {}", chat.chat, chat.place)
}

/// A package read for its import: the space, the repository it goes to,
/// its agents as they continue here and the session files they keep.
struct Package {
    space: Space,
    repository: crate::repository::Selector,
    sessions: Sessions,
    carried: Vec<ImportedSessionFile>,
    bundle: Option<tempfile::NamedTempFile>,
}

impl Package {
    /// Read the package in `file` for the repository with its origin, or
    /// the one `options` names, keeping the sessions `options` says.
    fn read(file: &Path, options: &ImportOptions) -> Result<Self> {
        let Import {
            manifest,
            sessions: files,
            bundle,
        } = read_import(file)?;
        let repository = options
            .repository
            .clone()
            .or_else(|| manifest.remote_url.clone())
            .context("the exported repository has no origin; select the repository to import into with --repo")?;
        let sessions = sessions_here(&manifest, options.sessions);
        let kept: BTreeSet<&str> = sessions.reported().map(|(_, session)| session).collect();
        let carried = files
            .into_iter()
            .filter(|imported| kept.contains(imported.file.session.as_str()))
            .collect();
        Ok(Self {
            space: manifest.space,
            repository: ui::repository_selector(repository)?,
            sessions,
            carried,
            bundle,
        })
    }

    /// Record the packaged workspace as removed in its repository and open
    /// it again there, putting its agents' session files in place.
    async fn open_workspace(self, ctx: &Context) -> Result<RestoredWorkspace> {
        let Space::Workspace(packaged) = &self.space else {
            anyhow::bail!("the package holds a chat, not a workspace");
        };
        let restored = ctx
            .progress(
                "Importing workspace",
                request::<Box<RestoredWorkspace>>(&ctx.paths, self.import_method(packaged)),
            )
            .await
            .map(|restored| *restored);
        // A workspace recorded but not opened keeps its sessions for `shoal
        // work restore`.
        let home = match &restored {
            Ok(restored) => SessionHome::Workspace(&restored.opened.workspace),
            Err(_) => SessionHome::Repository(self.repository.clone()),
        };
        agents::install_session_files(ctx, home, &self.carried).await;
        restored
    }

    /// The request that records `packaged` as removed and opens it again.
    fn import_method(&self, packaged: &PackagedWorkspace) -> RemovalMethod {
        RemovalMethod::ImportWorkspace {
            repository: self.repository.clone(),
            bundle: self.bundle.as_ref().map(|bundle| bundle.path().to_owned()),
            prerequisite: packaged.prerequisite.clone(),
            workspace: Box::new(packaged.workspace_with(self.sessions.clone())),
        }
    }

    /// Put the packaged chat's session files in place, then start the chat
    /// in its repository's checkout at `place`, under a free name, resuming
    /// the session its agent keeps here.
    async fn start_chat(self, ctx: &Context, place: ChatPlace) -> Result<Started> {
        let Space::Chat(packaged) = &self.space else {
            anyhow::bail!("the package holds a workspace, not a chat");
        };
        let repositories = client::repositories(&ctx.paths).await?;
        let repo = crate::repository::select(&repositories, self.repository.clone())
            .await?
            .clone();
        agents::install_session_files(
            ctx,
            SessionHome::Repository(self.repository.clone()),
            &self.carried,
        )
        .await;
        let settings = chat::settings(ctx, &repo).await?;
        let topic = chat::Topic {
            links: packaged.links.relations(),
            ..chat::Topic::repository(&repo)
        };
        let options = self.chat_options(ctx, packaged, place).await?;
        chat::launch(ctx, repo, settings, topic, options).await
    }

    /// How the packaged chat starts at `place`.
    async fn chat_options(
        &self,
        ctx: &Context,
        packaged: &PackagedChat,
        place: ChatPlace,
    ) -> Result<chat::Options> {
        let session = self
            .sessions
            .last_sessions
            .iter()
            .find_map(|last| last.session.clone());
        Ok(chat::Options {
            agent: Some(packaged.agent.parse()?),
            place,
            name: Some(client::free_chat_name(&ctx.paths, packaged.name.clone()).await?),
            model: Default::default(),
            args: Vec::new(),
            session,
            restore: None,
        })
    }
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

/// Import each of `files`, going on after one fails; chats start in the
/// background.
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
        match package.space {
            Space::Workspace(_) => import_workspace(ctx, file, package, options, &mut report).await,
            Space::Chat(_) => match package.start_chat(ctx, ChatPlace::Background).await {
                Ok(Started::Background(chat)) => {
                    report.imported(ctx, chat_message(&chat), Imported::Chat(chat))
                }
                Ok(Started::Exited(_)) => report.imported_quietly(),
                Err(error) => report.failed(ctx, file, &error),
            },
        }
    }
    report
}

/// Open the workspace `package` holds, from `file`, and set it up, queueing
/// its agents' restores once its post-setup hook succeeded when `options`
/// says.
async fn import_workspace(
    ctx: &Context,
    file: &Path,
    package: Package,
    options: &ImportOptions,
    report: &mut ImportReport,
) {
    let mut reopened = match open_and_prepare(ctx, package).await {
        Ok(reopened) => reopened,
        Err(error) => return report.failed(ctx, file, &error),
    };
    let workspace = reopened.workspace.clone();
    let post_setup = super::super::workspaces::run_post_setup(ctx, &workspace).await;
    if post_setup.is_ok() && options.resume {
        reopened.queue_agents(ctx).await;
    }
    let message = reopened.message(ctx, "Imported");
    report.imported(ctx, message, Imported::Workspace(Box::new(reopened)));
    if let Err(error) = post_setup {
        let error = error.context(format!(
            "imported {}, but its hooks.post_setup failed",
            workspace.name
        ));
        report.failed(ctx, file, &error);
    }
}

/// Open the workspace `package` holds and set it up.
async fn open_and_prepare(ctx: &Context, package: Package) -> Result<Reopened> {
    let restored = package.open_workspace(ctx).await?;
    super::super::restore::prepare_reopened(ctx, restored)
        .await?
        .context("its setup did not finish")
}

/// The spaces an import of several files continued, and the files it could
/// not import.
#[derive(Default)]
struct ImportReport {
    imported: Vec<Imported>,
    /// Spaces that reported themselves, such as a chat an app started.
    reported: usize,
    failed: Vec<ImportFailure>,
}

impl ImportReport {
    fn imported(&mut self, ctx: &Context, message: String, value: Imported) {
        if !ctx.json {
            outln!("{message}");
        }
        self.imported.push(value);
    }

    fn imported_quietly(&mut self) {
        self.reported += 1;
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
        let imported = self.imported.len() + self.reported;
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

/// The package's agents to import here, which keep their sessions as
/// `sessions` says, or on the machine that exported them.
fn sessions_here(manifest: &Manifest, sessions: Option<bool>) -> Sessions {
    manifest.sessions_on(crate::process::machine::id().as_deref(), sessions)
}

/// Read the export in `file`, or on standard input for `-`.
fn read_import(file: &Path) -> Result<Import> {
    if file.as_os_str() == STANDARD_STREAM {
        return Import::read(&mut std::io::stdin().lock());
    }
    let input = std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    Import::read(&mut BufReader::new(input))
}
