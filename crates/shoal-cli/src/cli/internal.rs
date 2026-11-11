//! Worker commands as shell text for the CLI's pickers to run.
use anyhow::Result;

use crate::{
    paths::Paths,
    workers::{Worker, internal_command},
};

/// [`internal_command`] as shell text, for a picker to run.
pub fn shell_command(paths: &Paths, worker: Worker<'_>) -> Result<String> {
    let words = internal_command(paths, false, worker)?
        .into_iter()
        .map(|word| word.into_string())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("the shoal executable or state path is not UTF-8"))?;
    Ok(crate::shell::quote(&words))
}

#[cfg(test)]
mod tests;
