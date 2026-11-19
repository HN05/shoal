//! How this machine's Git shows output another machine's Git wrote, such
//! as a diff: whether it colours it and the pager it pipes it into.
use std::path::Path;

use anyhow::Result;

use super::run;

/// Whether Git in `dir` colours a diff, as `color.diff`, else `color.ui`,
/// says for output to a terminal when `terminal`, else to a pipe.
pub async fn colors_diff(dir: &Path, terminal: bool) -> Result<bool> {
    let terminal = if terminal { "true" } else { "false" };
    let answer = run(dir, &["config", "--get-colorbool", "color.diff", terminal]).await?;
    Ok(answer.trim() == "true")
}

/// The pager Git in `dir` pipes `git diff`'s output into: none where
/// `pager.diff` turns paging off, the command it names unless `GIT_PAGER`
/// names one, as Git takes them, else the one `git var GIT_PAGER` names;
/// none where it pages nothing, as for `cat`.
pub async fn diff_pager(dir: &Path) -> Result<Option<String>> {
    let paging = super::queries::config_value(dir, "pager.diff").await?;
    let pager = match paging.as_deref().map(Paging::of) {
        Some(Paging::Off) => return Ok(None),
        Some(Paging::Command(command)) if std::env::var_os("GIT_PAGER").is_none() => {
            command.to_owned()
        }
        _ => run(dir, &["var", "GIT_PAGER"]).await?.trim().to_owned(),
    };
    Ok((!pager.is_empty() && pager != "cat").then_some(pager))
}

/// What a `pager.<command>` setting says: a boolean, as Git reads one, or
/// the command to page with.
#[derive(Debug, PartialEq, Eq)]
enum Paging<'a> {
    On,
    Off,
    Command(&'a str),
}

impl<'a> Paging<'a> {
    fn of(value: &'a str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" => Self::On,
            "false" | "no" | "off" => Self::Off,
            number => match number.parse::<i64>() {
                Ok(0) => Self::Off,
                Ok(_) => Self::On,
                Err(_) => Self::Command(value),
            },
        }
    }
}

#[cfg(test)]
mod tests;
