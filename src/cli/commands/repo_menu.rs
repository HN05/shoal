//! The bare `shoal repo` invocation: an fzf menu over registered repositories
//! that turns a selection plus key binding into an ordinary [`Command`].
use anyhow::{Result, ensure};

use crate::cli::{
    Command,
    args::{
        ConfirmationArgs,
        create::{CreateCommand, StartArgs},
        repository::{RepoCommand, RepositoryCommand},
    },
    context::Context,
    ui::{self, Key, KeyBindings},
};

const REGISTER_ENTRY: &str = "register-repository";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RepoMenuAction {
    AddWorkspace,
    Register,
    Rename,
    Config,
    Setup,
    Delete,
}

const BINDINGS: &[(Key, &str, RepoMenuAction)] = &[
    (Key::Enter, "add workspace", RepoMenuAction::AddWorkspace),
    (Key::Ctrl('a'), "register", RepoMenuAction::Register),
    (Key::Ctrl('r'), "rename", RepoMenuAction::Rename),
    (Key::Ctrl('o'), "config", RepoMenuAction::Config),
    (Key::Ctrl('s'), "set up", RepoMenuAction::Setup),
    (Key::Ctrl('d'), "delete", RepoMenuAction::Delete),
];

pub(super) async fn choose(ctx: &Context) -> Result<Command> {
    let mut entries =
        ui::repository_choices(crate::client::repositories::list(&ctx.paths).await?).await;
    entries.push((REGISTER_ENTRY.into(), "+ Register repository".into()));
    let picked = ui::pick_with_keys(ctx, "Repository> ", entries, KeyBindings(BINDINGS))?;
    let action = resolve_action(picked.action, &picked.id)?;
    Ok(match action {
        RepoMenuAction::Register => repo(RepoCommand::Add {
            source: ui::input(ctx, "Repository path or URL")?,
            name: None,
            path: None,
        }),
        RepoMenuAction::AddWorkspace => {
            Command::Create(CreateCommand::Start(Box::new(StartArgs {
                repository: Some(picked.id),
                mode: Some(crate::config::modes::DEFAULT.to_owned()),
                ..Default::default()
            })))
        }
        RepoMenuAction::Rename => repo(RepoCommand::Rename {
            repository: Some(picked.id),
            name: Some(ui::input(ctx, "New name")?),
        }),
        RepoMenuAction::Config => repo(RepoCommand::Config {
            repository: Some(picked.id),
        }),
        RepoMenuAction::Setup => repo(RepoCommand::Setup {
            repository: Some(picked.id),
            here: false,
            agent: None,
            model: Default::default(),
            args: vec![],
        }),
        RepoMenuAction::Delete => repo(RepoCommand::Rm {
            repository: Some(picked.id),
            confirmation: ConfirmationArgs::default(),
        }),
    })
}

/// Enter on the register entry registers; other actions need a repository.
fn resolve_action(action: RepoMenuAction, id: &str) -> Result<RepoMenuAction> {
    if id != REGISTER_ENTRY {
        return Ok(action);
    }
    ensure!(
        matches!(
            action,
            RepoMenuAction::AddWorkspace | RepoMenuAction::Register
        ),
        "select a repository for this action"
    );
    Ok(RepoMenuAction::Register)
}

fn repo(command: RepoCommand) -> Command {
    Command::Repository(RepositoryCommand::Repo {
        command: Some(command),
    })
}

#[cfg(test)]
mod tests;
