//! The bare `shoal` invocation: an fzf menu over workspaces that turns a
//! selection plus key binding into an ordinary [`Command`].
use anyhow::{Result, bail, ensure};

use crate::{
    cli::{Command, ConfirmationArgs},
    client,
    context::Context,
    env,
    ui::{self, KeyBindings},
};

const ADD_ENTRY: &str = "add-workspace";

const SCOPED_BINDINGS: KeyBindings = KeyBindings {
    keys: "ctrl-e,ctrl-o,ctrl-f",
    header: "enter: enter   ctrl-e: execute   ctrl-o: inspect   ctrl-f: diff",
};
const FULL_BINDINGS: KeyBindings = KeyBindings {
    keys: "ctrl-d,ctrl-e,ctrl-a,ctrl-o,ctrl-s,ctrl-f",
    header: "enter: enter   ctrl-d: delete   ctrl-e: execute   ctrl-a: add   ctrl-o: inspect   ctrl-s: stop   ctrl-f: diff",
};

pub(super) async fn choose(ctx: &Context) -> Result<Command> {
    let repos = ui::repository_choices(client::repositories(&ctx.paths).await?).await?;
    let mut entries: Vec<_> = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .map(|w| {
            let repo = repos
                .iter()
                .find(|(id, _)| id == &w.repository_id)
                .map(|(_, name)| name.as_str())
                .unwrap_or("unknown repository");
            (
                w.id,
                format!("{}  {repo}  {}  {}", w.name, w.state, w.branch),
            )
        })
        .collect();
    let scoped = env::is_scoped();
    if !scoped {
        entries.push((ADD_ENTRY.into(), "+ Add workspace".into()));
    }
    let bindings = if scoped {
        SCOPED_BINDINGS
    } else {
        FULL_BINDINGS
    };
    let picked = ui::pick_with_keys(ctx, "Shoal> ", entries, Some(bindings))?;
    if picked.key == "ctrl-a" || (picked.key.is_empty() && picked.id == ADD_ENTRY) {
        return Ok(Command::Add {
            path: None,
            repository: None,
            branch: None,
            existing: None,
            issue: None,
            base: None,
            git_profile: None,
            agent: None,
            args: vec![],
        });
    }
    ensure!(picked.id != ADD_ENTRY, "select a workspace for this action");
    let workspace = Some(picked.id);
    Ok(match picked.key.as_str() {
        "" => Command::Cd { workspace },
        "ctrl-d" => Command::Rm {
            workspace,
            confirmation: ConfirmationArgs::default(),
            keep_branch: false,
            delete_branch: false,
        },
        "ctrl-o" => Command::Inspect { workspace },
        "ctrl-s" => Command::Stop { workspace },
        "ctrl-f" => Command::Diff { workspace },
        "ctrl-e" => execute_command(ctx, workspace)?,
        _ => bail!("unknown picker action"),
    })
}

fn execute_command(ctx: &Context, workspace: Option<String>) -> Result<Command> {
    const CHOICES: [&str; 7] = [
        "claude",
        "codex cli",
        "codex app",
        "happy claude",
        "happy codex",
        "t3",
        "custom shell command",
    ];
    let choice = ui::pick(
        ctx,
        "Execute> ",
        CHOICES
            .iter()
            .map(|s| (s.to_string(), s.to_string()))
            .collect(),
    )?;
    Ok(match choice.as_str() {
        "claude" => Command::Claude {
            workspace,
            args: vec![],
        },
        "codex cli" | "codex app" => Command::Codex {
            workspace,
            cli: choice == "codex cli",
            app: choice == "codex app",
            args: vec![],
        },
        "happy claude" | "happy codex" => Command::Happy {
            agent: <crate::happy::HappyAgent as clap::ValueEnum>::from_str(
                choice.trim_start_matches("happy "),
                false,
            )
            .map_err(|error| anyhow::anyhow!(error))?,
            workspace,
            prompt: None,
            args: vec![],
        },
        "t3" => Command::T3 {
            workspace,
            args: vec![],
        },
        _ => Command::Exec {
            workspace,
            command: vec![
                "sh".into(),
                "-c".into(),
                ui::input(ctx, "Shell command")?.into(),
            ],
        },
    })
}
