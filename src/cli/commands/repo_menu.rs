//! The bare `shoal repo` invocation: an fzf menu over registered repositories
//! that turns a selection plus key binding into an ordinary [`Command`].
use anyhow::{Result, ensure};

use crate::cli::{
    Command, ConfirmationArgs, RepoCommand, client,
    context::Context,
    ui::{self, KeyBindings},
};

const REGISTER_ENTRY: &str = "register-repository";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RepoMenuAction {
    AddWorkspace,
    Register,
    Rename,
    Config,
    Delete,
}

const BINDINGS: &[(&str, &str, RepoMenuAction)] = &[
    ("enter", "add workspace", RepoMenuAction::AddWorkspace),
    ("ctrl-a", "register", RepoMenuAction::Register),
    ("ctrl-r", "rename", RepoMenuAction::Rename),
    ("ctrl-o", "config", RepoMenuAction::Config),
    ("ctrl-d", "delete", RepoMenuAction::Delete),
];

pub(super) async fn choose(ctx: &Context) -> Result<Command> {
    let mut entries = ui::repository_choices(client::repositories(&ctx.paths).await?).await?;
    entries.push((REGISTER_ENTRY.into(), "+ Register repository".into()));
    let picked = ui::pick_with_keys(ctx, "Repository> ", entries, KeyBindings(BINDINGS))?;
    let action = resolve_action(picked.action, &picked.id)?;
    Ok(match action {
        RepoMenuAction::Register => repo(RepoCommand::Add {
            source: ui::input(ctx, "Repository path or URL")?,
            name: None,
            path: None,
        }),
        RepoMenuAction::AddWorkspace => Command::Add {
            here: false,
            path: None,
            repository: Some(picked.id),
            repository_override: None,
            branch: None,
            existing: None,
            issue: None,
            base: None,
            git_profile: None,
            agent: None,
            args: vec![],
        },
        RepoMenuAction::Rename => repo(RepoCommand::Rename {
            repository: picked.id,
            name: ui::input(ctx, "New name")?,
        }),
        RepoMenuAction::Config => repo(RepoCommand::Config {
            repository: picked.id,
            file: None,
            clear: false,
        }),
        RepoMenuAction::Delete => repo(RepoCommand::Rm {
            repository: picked.id,
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
    Command::Repo {
        command: Some(command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_entry_accepts_only_registration() {
        for action in [RepoMenuAction::AddWorkspace, RepoMenuAction::Register] {
            assert_eq!(
                resolve_action(action, REGISTER_ENTRY).unwrap(),
                RepoMenuAction::Register
            );
        }
        for action in [
            RepoMenuAction::Rename,
            RepoMenuAction::Config,
            RepoMenuAction::Delete,
        ] {
            assert!(resolve_action(action, REGISTER_ENTRY).is_err());
            assert_eq!(resolve_action(action, "repo-id").unwrap(), action);
        }
    }
}
