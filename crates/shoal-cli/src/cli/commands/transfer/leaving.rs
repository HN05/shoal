//! A shell inside the worktree of a workspace a move ended goes elsewhere once
//! it is gone.
use anyhow::Result;
use std::path::PathBuf;

use crate::cli::{context::Context, shell};

pub(super) struct Leaving(Option<PathBuf>);

impl Leaving {
    pub(super) async fn find(ctx: &Context) -> Result<Self> {
        for space in crate::client::spaces::list(&ctx.paths).await?.iter() {
            let escape = super::super::removal::escape_destination(ctx, space).await?;
            if escape.is_some() {
                return Ok(Self(escape));
            }
        }
        Ok(Self(None))
    }

    pub(super) fn navigate(self, ctx: &Context) -> Result<()> {
        if let Some(destination) = self.0
            && !std::env::current_dir().is_ok_and(|cwd| cwd.exists())
        {
            shell::navigate(ctx, &destination)?;
        }
        Ok(())
    }
}
