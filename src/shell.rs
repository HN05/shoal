use anyhow::{Context, Result, ensure};
use std::path::Path;

/// Directory changes use a private data file, never shell code evaluated from
/// a repository path or command output. The wrapper works in Bash and Zsh.
pub const INIT: &str = r#"shoal() {
  local shoal_cd_file shoal_exit shoal_destination
  shoal_cd_file="$(mktemp "${TMPDIR:-/tmp}/shoal-cd.XXXXXXXX")" || return 1
  if SHOAL_SHELL_DIRECTIVE="$shoal_cd_file" command shoal "$@"; then
    shoal_exit=0
  else
    shoal_exit=$?
  fi
  if IFS= read -r shoal_destination < "$shoal_cd_file"; then
    if [ -n "$shoal_destination" ]; then
      if ! builtin cd -- "$shoal_destination"; then
        command rm -f -- "$shoal_cd_file"
        return 1
      fi
    fi
  fi
  command rm -f -- "$shoal_cd_file"
  return "$shoal_exit"
}
"#;

pub fn navigate(path: &Path, json: bool) -> Result<()> {
    if json {
        return Ok(());
    }
    if let Some(destination) = std::env::var_os("SHOAL_SHELL_DIRECTIVE") {
        let path = path
            .to_str()
            .context("shell navigation path is not UTF-8")?;
        ensure!(
            !path.contains(['\n', '\r']),
            "shell navigation does not support newlines in paths"
        );
        std::fs::write(destination, format!("{path}\n")).context("write shell directory change")?;
    }
    Ok(())
}
