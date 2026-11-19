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
    let pager = match diff_paging(dir).await? {
        Paging::Off => return Ok(None),
        Paging::Command(command) if std::env::var_os("GIT_PAGER").is_none() => command,
        _ => run(dir, &["var", "GIT_PAGER"]).await?.trim().to_owned(),
    };
    Ok((!pager.is_empty() && pager != "cat").then_some(pager))
}

/// What `pager.diff` says.
enum Paging {
    /// Page as `GIT_PAGER` and `core.pager` say, as without the setting.
    Default,
    Off,
    Command(String),
}

/// What `pager.diff` says in `dir`: a boolean where Git reads one, an
/// empty value as false and one without a value as true, else the command
/// to page with.
async fn diff_paging(dir: &Path) -> Result<Paging> {
    const KEY: &str = "pager.diff";
    let mut setting = super::command(dir);
    setting.args(["config", "--get", KEY]);
    let Some(setting) = super::output_unless(setting, 1).await? else {
        return Ok(Paging::Default);
    };
    let boolean = run(dir, &["config", "--type=bool", "--get", KEY]).await;
    Ok(match boolean {
        Ok(boolean) if boolean.trim() == "false" => Paging::Off,
        Ok(_) => Paging::Default,
        Err(_) => Paging::Command(setting.trim_end_matches('\n').to_owned()),
    })
}

#[cfg(test)]
mod tests;
