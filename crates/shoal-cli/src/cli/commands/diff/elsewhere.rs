//! `shoal diff` of a workspace on another joined machine: that machine's Git
//! writes the diff, and this one's decides its colour and pager.
use std::{
    io::{IsTerminal as _, Write as _},
    path::Path,
    process::Stdio,
};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    diffs::{self, DiffDisplay, DiffOutput},
    space_names::SpaceSelector,
};
use tokio::{
    io::AsyncWriteExt as _,
    process::{Child, ChildStdin, Command},
};

use crate::{cli::context::Context, git};

pub(super) async fn diff(ctx: &Context, workspace: SpaceSelector) -> Result<i32> {
    let here = std::env::current_dir().context("read the current directory")?;
    let terminal = std::io::stdout().is_terminal();
    let display = display(&here, terminal).await?;
    let mut diff = crate::client::diffs::diff(&ctx.paths, workspace, display).await?;
    let pager = match terminal {
        true => git::display::diff_pager(&here).await?,
        false => None,
    };
    let mut output = Output::open(pager)?;
    loop {
        match diff.next().await? {
            DiffOutput::Stdout(data) => {
                if !output.write(&diffs::decode(&data)?).await? {
                    // The pager quit, as Git's does when its user is done.
                    return output.close().await.map(|()| 0);
                }
            }
            DiffOutput::Stderr(data) => std::io::stderr().write_all(&diffs::decode(&data)?)?,
            DiffOutput::Exited(code) => return output.close().await.map(|()| code),
        }
    }
}

async fn display(here: &Path, terminal: bool) -> Result<DiffDisplay> {
    let columns = match terminal {
        true => shoal_core::execution::terminal_width(std::io::stdout()),
        false => None,
    };
    Ok(DiffDisplay {
        color: git::display::colors_diff(here, terminal).await?,
        columns: columns.map(|columns| u16::try_from(columns).unwrap_or(u16::MAX)),
    })
}

enum Output {
    Stdout(std::io::Stdout),
    Pager(Box<Child>, ChildStdin),
}

impl Output {
    /// Start `pager`, as Git does, with Git's defaults for `less` and `lv`;
    /// standard output without one.
    fn open(pager: Option<String>) -> Result<Self> {
        let Some(pager) = pager else {
            return Ok(Self::Stdout(std::io::stdout()));
        };
        let mut command = Command::new("sh");
        command.args(["-c", &pager]).stdin(Stdio::piped());
        for (variable, default) in [("LESS", "FRX"), ("LV", "-c")] {
            if std::env::var_os(variable).is_none() {
                command.env(variable, default);
            }
        }
        let mut child = (command.spawn()).with_context(|| format!("start the pager {pager}"))?;
        let input = child.stdin.take().context("the pager's input")?;
        Ok(Self::Pager(Box::new(child), input))
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<bool> {
        let written = match self {
            Self::Stdout(stdout) => stdout.write_all(bytes),
            Self::Pager(_, input) => input.write_all(bytes).await,
        };
        match written {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
            Err(error) => Err(error).context("write the diff"),
        }
    }

    async fn close(self) -> Result<()> {
        match self {
            Self::Stdout(mut stdout) => Ok(stdout.flush()?),
            Self::Pager(mut child, input) => {
                drop(input);
                child.wait().await.context("wait for the pager")?;
                Ok(())
            }
        }
    }
}
