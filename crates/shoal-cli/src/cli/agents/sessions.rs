//! Agents' session files that `shoal restore --from` puts where the agents
//! look for them.
use shoal_protocol::transcripts::CarriedSessionFile;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};

use crate::{
    agent::Agent,
    cli::{agents::environment, context::Context},
    config::Effective,
    model::{Chat, RemovedSpace, Space, Workspace},
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::{self, AgentEnvironment};

/// Whose agents an import's session files go to.
pub(in crate::cli) enum SessionHome<'a> {
    /// The workspace the import or restore opened.
    Workspace(&'a Workspace),
    /// The chat the import or restore opened.
    Chat(&'a Chat),
}

/// What an import does with a session file already where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli) enum Existing {
    /// Replace it with one that continues it, as a session that went to
    /// another machine and came back does.
    Continued,
    /// Keep it, whatever the import holds, as a copy's import does: its
    /// session may still run here, writing to it.
    Kept,
}

impl Existing {
    /// What the import of a space does, a `copy` or not.
    pub(in crate::cli) fn of_copy(copy: bool) -> Self {
        match copy {
            true => Self::Kept,
            false => Self::Continued,
        }
    }
}

/// Put the session files an import carried where the agents of `home` look
/// for them, as [`place`] does with the files there as `existing` says. A
/// development build leaves them to the installed Shoal. Failures only
/// warn; that agent's resume then fails to find its session. Returns
/// whether every file is in place.
pub(in crate::cli) async fn install_session_files(
    ctx: &Context,
    home: SessionHome<'_>,
    files: &[CarriedSessionFile],
    existing: Existing,
) -> bool {
    if files.is_empty() {
        return true;
    }
    let space = match home {
        SessionHome::Workspace(workspace) => Space::from(workspace.clone()),
        SessionHome::Chat(chat) => Space::from(chat.clone()),
    };
    let space_environment = environment::agent_environment(ctx, &space).await;
    let target = ConfigTarget::space(&space);
    place_session_files(ctx, (target, &space_environment), files, existing).await
}

/// Put `files` where the agents look for them under the settings of
/// `target` with its space's `[env]`, `space_environment`, as
/// [`place`] does with the files there as `existing` says. A development
/// build leaves them to the installed Shoal. Failures only warn; that
/// agent's start from the session then fails to find it. Returns whether
/// every file is in place.
pub(in crate::cli) async fn place_session_files(
    ctx: &Context,
    (target, space_environment): (ConfigTarget, &BTreeMap<String, OsString>),
    files: &[CarriedSessionFile],
    existing: Existing,
) -> bool {
    if !crate::build::changes_user_files() {
        return false;
    }
    let settings = match shoal_workers::settings::of(&ctx.paths, target).await {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("warning: cannot place the agents' session files: {error:#}");
            return false;
        }
    };
    let mut directories = BTreeMap::new();
    let mut placed_all = true;
    for imported in files {
        let agent = &imported.file.agent;
        let directory = directories
            .entry(agent.clone())
            .or_insert_with(|| session_dir(&settings, space_environment, &ctx.paths.home, agent));
        let placed = match directory {
            Ok(Some(directory)) => place(directory, imported, existing),
            Ok(None) => Err(anyhow::anyhow!("{agent} keeps no session files")),
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        if let Err(error) = placed {
            placed_all = false;
            eprintln!(
                "warning: cannot place {} of {agent} session {}: {error:#}",
                imported.file.path.display(),
                imported.file.session
            );
        }
    }
    placed_all
}

/// `shoal internal place-sessions`: put the session files whose list,
/// with where each one's contents are, stdin holds where the agents look
/// for them under the settings of `repository`, else of the global
/// config, keeping one already there that the carried one does not
/// continue. Exits 1 when a file could not be placed, so the fork that
/// needs it is refused; a development build leaves them to the installed
/// Shoal.
pub(in crate::cli) async fn place_forked_sessions(
    ctx: &Context,
    repository: Option<shoal_ids::repository_ids::RepositoryId>,
) -> Result<i32> {
    let files: Vec<CarriedSessionFile> = serde_json::from_reader(std::io::stdin().lock())?;
    if !crate::build::changes_user_files() {
        eprintln!(
            "warning: a development build leaves the forked session's files to the installed Shoal"
        );
        return Ok(0);
    }
    let target = match repository {
        Some(repository) => ConfigTarget::Repository(repository.into()),
        None => ConfigTarget::Global,
    };
    let under = (target, &BTreeMap::new());
    Ok(
        match place_session_files(ctx, under, &files, Existing::Continued).await {
            true => 0,
            false => 1,
        },
    )
}

/// Put `files`, which the daemon kept with the removal record `restored`,
/// where the agents of `space` that restores it look for them, then have
/// the daemon delete them once they all are.
pub(in crate::cli) async fn install_kept_session_files(
    ctx: &Context,
    space: SessionHome<'_>,
    restored: &RemovedSpace,
    files: &[CarriedSessionFile],
) {
    if files.is_empty() {
        return;
    }
    let selector = match &space {
        SessionHome::Workspace(workspace) => SpaceSelector::id(&workspace.id),
        SessionHome::Chat(chat) => SpaceSelector::id(&chat.id),
    };
    let existing = Existing::of_copy(restored.is_copy());
    // Left behind, the space's next removal record lists them again.
    if !install_session_files(ctx, space, files, existing).await {
        return;
    }
    if let Err(error) = crate::client::removal::drop_restored_sessions(&ctx.paths, selector).await {
        eprintln!("warning: cannot delete the kept session files: {error:#}");
    }
}

/// Copy the files of `session`, which agent `name` under `settings` starts
/// from a fork of in `space`, where its tool looks for them under the
/// space's `[env]`, from where it keeps them without it, as for a
/// session another machine carried here. A failure only warns; the fork
/// then starts from what that directory has.
pub(in crate::cli) async fn find_forked_session(
    ctx: &Context,
    space: &crate::agents::HomeRecords,
    (settings, name): (&Effective, &Agent),
    session: &shoal_ids::agent_session_ids::AgentSessionId,
) {
    if !crate::build::changes_user_files() {
        return;
    }
    let Some(store) = transcripts::store(settings.integration(name)) else {
        return;
    };
    let space_environment = match space {
        crate::agents::HomeRecords::Workspace(workspace) => {
            environment::agent_environment(ctx, &Space::from(workspace.clone())).await
        }
        crate::agents::HomeRecords::Chat { chat, .. } => {
            environment::agent_environment(ctx, &Space::from(chat.clone())).await
        }
    };
    let home = &ctx.paths.home;
    let directories = (
        session_dir(settings, &space_environment, home, name),
        session_dir(settings, &BTreeMap::new(), home, name),
    );
    let (Ok(Some(directory)), Ok(Some(default))) = directories else {
        return;
    };
    if let Err(error) = copy_session(store, (&default, &directory), (name, session)) {
        eprintln!("warning: cannot find {name} session {session} where the fork looks: {error:#}");
    }
}

/// Copy the files of `session` of agent `name` that `store` keeps in
/// `default` into `directory`, unless that is where they are, replacing a
/// copy there that they continue, as after the session was carried again,
/// and keeping one that went its own way.
fn copy_session(
    store: &dyn transcripts::Store,
    (default, directory): (&Path, &Path),
    (name, session): (&Agent, &shoal_ids::agent_session_ids::AgentSessionId),
) -> Result<()> {
    if default == directory {
        return Ok(());
    }
    for path in store.session_files(default, session.as_str())?.files {
        let file = transcripts::SessionFile {
            agent: name.clone(),
            session: session.clone(),
            size: 0,
            path,
        };
        let contents = default.join(&file.path);
        place(
            directory,
            &CarriedSessionFile { file, contents },
            Existing::Continued,
        )?;
    }
    Ok(())
}

/// The directory agent `name`, as `settings` describes it, keeps its
/// sessions' files in when it runs with its space's `[env]`,
/// `space_environment`, under `home`.
fn session_dir(
    settings: &Effective,
    space_environment: &BTreeMap<String, OsString>,
    home: &Path,
    name: &Agent,
) -> Result<Option<PathBuf>> {
    let mut configured = space_environment.clone();
    settings
        .launch(name)
        .environment
        .layer(&mut configured, home);
    let environment = AgentEnvironment::new(home, &configured);
    transcripts::store(settings.integration(name))
        .map(|store| store.session_dir(&environment))
        .transpose()
}

/// Copy `imported` into the session directory `directory`. A file already
/// there stays unless `existing` lets the imported one replace it and it
/// continues the file, as a session that went to another machine and came
/// back does; one that differs otherwise is a conflict, which keeps it and
/// warns.
fn place(directory: &Path, imported: &CarriedSessionFile, existing: Existing) -> Result<()> {
    let destination = directory.join(&imported.file.path);
    let replaces = destination.exists();
    if replaces && existing == Existing::Kept {
        return Ok(());
    }
    if replaces {
        ensure!(
            continues(&destination, &imported.contents)?,
            "{} differs from the session's file here, which stays",
            destination.display()
        );
    }
    let parent = destination.parent().unwrap_or(directory);
    std::fs::create_dir_all(parent)?;
    let partial = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::copy(&imported.contents, partial.path())?;
    match replaces {
        true => partial.persist(&destination)?,
        false => partial.persist_noclobber(&destination)?,
    };
    Ok(())
}

/// Whether the file `imported` starts with all of `existing`, as a
/// transcript does once its session went on.
fn continues(existing: &Path, imported: &Path) -> std::io::Result<bool> {
    let existing_size = std::fs::metadata(existing)?.len();
    if existing_size > std::fs::metadata(imported)?.len() {
        return Ok(false);
    }
    let mut existing = std::io::BufReader::new(std::fs::File::open(existing)?);
    let mut imported = std::io::BufReader::new(std::fs::File::open(imported)?);
    let mut left = [0; 8192];
    let mut right = [0; 8192];
    loop {
        let read = existing.read(&mut left)?;
        if read == 0 {
            return Ok(true);
        }
        imported.read_exact(&mut right[..read])?;
        if left[..read] != right[..read] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests;
