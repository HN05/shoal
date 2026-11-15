//! `shoal move import`: continue spaces from the packages `shoal move
//! export` wrote: open workspaces again and start chats that resume their
//! sessions.
use crate::cli::commands::restore::reopen::ReopenedExt;
use crate::transfer::ManifestExt;
use crate::transfer::PackagedWorkspaceExt;
use std::{
    collections::BTreeSet,
    io::BufReader,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};

use crate::client::output::chat::BackgroundChat;
use crate::client::output::{
    restore::Reopened,
    transfer::{ImportFailure, Imported, Imports},
};
use crate::{
    cli::{
        agents::{self, ChatRepository, SessionHome},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    execution::recovery::Copied,
    model::RestoredWorkspace,
    refusal::{Reason, Refusal},
    transfer::{
        CarriedSessionFile, Import, ImportedSessionFile, Manifest, PackagedChat, PackagedWorkspace,
        Sessions, Space,
    },
};
use shoal_ids::agent_session_ids::AgentSessionId;

use super::{
    super::chat::{self, ChatPlace, Started},
    STANDARD_STREAM,
};

mod chats;

use chats::ChatImport;

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
    pub restore: bool,
}

/// `shoal move import`: continue the spaces exported to `files`, or to the
/// `.shoal` files in the directories among them, as `options` says. A
/// directory or several files import each space and report every result.
pub(super) async fn import(
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
        return match package.space {
            Space::Workspace(_) => {
                let restored = package.open_workspace(ctx).await?;
                let reopened = super::super::restore::reopen::finish_reopen(
                    ctx,
                    restored,
                    "Imported",
                    options.restore,
                )
                .await;
                package.open_chats(ctx, options.restore).await;
                reopened
            }
            // A chat starts on this terminal, unless it is to start in the
            // background or there is none to start it on.
            Space::Chat(_) => {
                let place = chat_place(ctx, &options);
                match package.start_chat(ctx, place, options.restore).await? {
                    Started::Exited(code) => Ok(code),
                    Started::Background(chat) => {
                        ctx.emit(&chat_message(&chat), &chat)?;
                        Ok(0)
                    }
                }
            }
        };
    }
    let files = import_files(files)?;
    import_each(ctx, &files, &options).await.report(ctx)
}

/// Where a chat imported on its own starts: beside this terminal, unless
/// `--restore` starts it in the background, as it must without a terminal.
fn chat_place(ctx: &Context, options: &ImportOptions) -> ChatPlace {
    match options.restore || !ctx.on_terminal() {
        true => ChatPlace::Background,
        false => ChatPlace::Beside,
    }
}

/// The report of an imported chat started in the background.
fn chat_message(chat: &BackgroundChat) -> String {
    format!("Imported chat {}. {}", chat.chat, chat.place)
}

/// A package read for its import: the space, the repository it goes to,
/// its agents as they continue here, the chats that go with it and the
/// session files they keep.
struct Package {
    space: Space,
    /// Set for a copy, which opens as a new space.
    copy: Option<Copied>,
    /// None for a chat without a repository.
    repository: Option<crate::repository::Selector>,
    sessions: Sessions,
    chats: Vec<ChatImport>,
    carried: Vec<ImportedSessionFile>,
    bundle: Option<tempfile::NamedTempFile>,
}

impl Package {
    /// Read the package in `file` for the repository with its origin, or
    /// the one `options` names, unless it holds a chat without a
    /// repository, keeping the sessions `options` says.
    fn read(file: &Path, options: &ImportOptions) -> Result<Self> {
        let Import {
            manifest,
            sessions: files,
            bundle,
        } = read_import(file)?;
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
        let repository = match &manifest.space {
            Space::Chat(chat) if chat.outside_repositories => None,
            _ => Some(destination(&manifest, options)?),
        };
        let sessions = sessions_here(&manifest, options.sessions);
        let chats = chats::chats_here(&manifest, repository.as_ref(), options.sessions);
        let kept: BTreeSet<&AgentSessionId> = (sessions.reported())
            .map(|(_, session)| session)
            .chain(chats.iter().flat_map(ChatImport::sessions))
            .collect();
        let carried = files
            .into_iter()
            .filter(|imported| kept.contains(&imported.file.session))
            .collect();
        Ok(Self {
            space: manifest.space,
            copy: manifest.copy,
            repository,
            sessions,
            chats,
            carried,
            bundle,
        })
    }

    /// Open again the chats that went with the space, queueing the restores
    /// of their agents when `restore`.
    async fn open_chats(&self, ctx: &Context, restore: bool) {
        chats::open(ctx, &self.chats, &self.carried(), restore).await;
    }

    /// Record the packaged workspace as removed in its repository, keeping
    /// its agents' session files with the record for the restore that opens
    /// it, and open it again.
    async fn open_workspace(&self, ctx: &Context) -> Result<RestoredWorkspace> {
        let Space::Workspace(packaged) = &self.space else {
            anyhow::bail!("the package holds a chat, not a workspace");
        };
        ctx.progress("Importing workspace", self.import(ctx, packaged))
            .await
            .map(|restored| *restored)
    }

    /// Record `packaged` as removed with its session files and open it
    /// again.
    async fn import(
        &self,
        ctx: &Context,
        packaged: &PackagedWorkspace,
    ) -> Result<Box<RestoredWorkspace>> {
        let repository =
            (self.repository.clone()).context("a workspace belongs to a repository")?;
        crate::client::transfer::import(
            &ctx.paths,
            repository,
            self.bundle.as_ref().map(|bundle| bundle.path().to_owned()),
            packaged.prerequisite.clone(),
            packaged.workspace_with(self.sessions.clone()),
            self.copy.is_some(),
            self.carried(),
        )
        .await
    }

    /// The session files the package carries for the agents kept here.
    fn carried(&self) -> Vec<CarriedSessionFile> {
        self.carried
            .iter()
            .map(ImportedSessionFile::carried)
            .collect()
    }

    /// Start the packaged chat at `place`, as [`Self::chat_start`] says,
    /// under its name, resuming the session its agent keeps here, once its
    /// session files are in place and the chats that go with it opened,
    /// queueing their agents' restores when `restore`.
    async fn start_chat(self, ctx: &Context, place: ChatPlace, restore: bool) -> Result<Started> {
        // Nothing is recorded yet, so a program that moves the chat knows it
        // is not here.
        let start = self
            .chat_start(ctx, place)
            .await
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        // The chat may run in this terminal until it ends, so the chats that
        // go with it open first.
        self.open_chats(ctx, restore).await;
        self.place_chat_sessions(ctx).await;
        let ChatStart {
            repo,
            settings,
            topic,
            options,
        } = start;
        chat::launch(ctx, repo, settings, topic, options).await
    }

    /// Put the session files of the packaged chat's agent where it looks for
    /// them.
    async fn place_chat_sessions(&self, ctx: &Context) {
        let home = match &self.repository {
            Some(selector) => SessionHome::Repository(selector.clone()),
            None => SessionHome::Root,
        };
        let existing = agents::Existing::of_copy(self.copy.is_some());
        agents::install_session_files(ctx, home, &self.carried(), existing).await;
    }

    /// How the packaged chat starts at `place`, in the repository the
    /// package goes to, which this machine registers, or in `root_dir`.
    async fn chat_start(&self, ctx: &Context, place: ChatPlace) -> Result<ChatStart> {
        let Space::Chat(packaged) = &self.space else {
            anyhow::bail!("the package holds a workspace, not a chat");
        };
        let repo = self.chat_repository(ctx).await?;
        let workspace = hosting_workspace(ctx, packaged, &repo).await;
        let (mode, links) = (&packaged.mode, &packaged.links);
        Ok(ChatStart {
            settings: crate::config::settings(&ctx.paths, repo.config()).await?,
            topic: chat::Topic::imported(&repo, workspace, mode, links),
            options: self.chat_options(packaged, place)?,
            repo,
        })
    }

    /// Where the packaged chat starts: the checkout of the registered
    /// repository the package goes to, or `root_dir`.
    async fn chat_repository(&self, ctx: &Context) -> Result<ChatRepository> {
        let Some(selector) = &self.repository else {
            return Ok(ChatRepository::Outside(chat::root_dir(ctx).await?));
        };
        let repositories = crate::client::repositories::list(&ctx.paths).await?;
        let repo = crate::repository::select(&repositories, selector.clone()).await?;
        Ok(ChatRepository::Registered(repo.clone()))
    }

    /// How the packaged chat starts at `place`.
    fn chat_options(&self, packaged: &PackagedChat, place: ChatPlace) -> Result<chat::Options> {
        let session = self
            .sessions
            .last_sessions
            .iter()
            .find_map(|last| last.session.clone());
        Ok(chat::Options {
            now: false,
            priority: None,
            narrowing: chat::Narrowing::Recorded(packaged.narrowing),
            agent: Some(packaged.agent.name().parse()?),
            place,
            // An older Shoal's export may hold a longer name than one set
            // now allows.
            name: Some(crate::client::space_names::SpaceName::derive(
                packaged.name.as_str(),
                "chat",
            )),
            model: Default::default(),
            args: Vec::new(),
            session,
            fork: self.copy.is_some(),
            handoff: None,
            parent: None,
        })
    }
}

/// What an imported chat starts with.
struct ChatStart {
    repo: ChatRepository,
    settings: crate::config::Effective,
    topic: chat::Topic,
    options: chat::Options,
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
            Space::Workspace(_) => {
                if import_workspace(ctx, file, &package, options, &mut report).await {
                    package.open_chats(ctx, options.restore).await;
                }
            }
            Space::Chat(_) => {
                let started = package.start_chat(ctx, ChatPlace::Background, options.restore);
                match started.await {
                    Ok(Started::Background(chat)) => {
                        report.imported(ctx, chat_message(&chat), Imported::Chat(chat))
                    }
                    Ok(Started::Exited(_)) => report.imported_quietly(),
                    Err(error) => report.failed(ctx, file, &error),
                }
            }
        }
    }
    report
}

/// Open the workspace `package` holds, from `file`, and set it up, queueing
/// its agents' restores once its post-setup hook succeeded when `options`
/// says; whether it opened.
async fn import_workspace(
    ctx: &Context,
    file: &Path,
    package: &Package,
    options: &ImportOptions,
    report: &mut ImportReport,
) -> bool {
    let mut reopened = match open_and_prepare(ctx, package).await {
        Ok(reopened) => reopened,
        Err(error) => {
            report.failed(ctx, file, &error);
            return false;
        }
    };
    let Some(workspace) = reopened.space.worktree().cloned() else {
        let error = anyhow::anyhow!("the import opened no workspace");
        report.failed(ctx, file, &error);
        return false;
    };
    let post_setup = super::super::workspaces::run_post_setup(ctx, &workspace).await;
    if post_setup.is_ok() && options.restore {
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
    true
}

/// Open the workspace `package` holds and set it up.
async fn open_and_prepare(ctx: &Context, package: &Package) -> Result<Reopened> {
    let restored = package.open_workspace(ctx).await?;
    super::super::restore::reopen::prepare_reopened(ctx, restored)
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

/// The workspace of `repo` in whose worktree the `packaged` chat ran, when
/// this machine has it ready, such as one that moved here before; without
/// it, the chat starts in the checkout, with a warning.
async fn hosting_workspace(
    ctx: &Context,
    packaged: &PackagedChat,
    repo: &ChatRepository,
) -> Option<crate::model::Workspace> {
    let ran_in = packaged.workspace.as_ref()?;
    let selector = crate::client::space_names::SpaceSelector::id(&ran_in.id);
    let found = chat::hosting_workspace(ctx, &selector).await.ok();
    let in_repository = |workspace: &crate::model::Workspace| {
        (repo.registered()).is_some_and(|repo| repo.id == workspace.repository_id)
    };
    let workspace = found.filter(in_repository);
    if workspace.is_none() {
        eprintln!(
            "warning: chat {} ran in workspace {}, which is not ready here, so it starts in the checkout",
            packaged.name, ran_in.name
        );
    }
    workspace
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

/// Read the export in `file`, or on standard input for `-`.
fn read_import(file: &Path) -> Result<Import> {
    if file.as_os_str() == STANDARD_STREAM {
        return Import::read(&mut std::io::stdin().lock());
    }
    let input = std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    Import::read(&mut BufReader::new(input))
}

#[cfg(test)]
mod tests;
