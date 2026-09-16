use anyhow::{Context, Result, ensure};
use std::path::Path;

pub const INIT_COMMAND: &str = "source <(shoal shell init)";

/// Directory changes use a private data file, never shell code evaluated from
/// a repository path or command output. The wrapper works in Bash and Zsh.
pub const INIT: &str = r#"shoal() {
  local shoal_cd_file shoal_destination shoal_exit=0
  shoal_cd_file="$(mktemp "${TMPDIR:-/tmp}/shoal-cd.XXXXXXXX")" || return 1
  SHOAL_SHELL_DIRECTIVE="$shoal_cd_file" command shoal "$@" || shoal_exit=$?
  IFS= read -r shoal_destination < "$shoal_cd_file" || :
  command rm -f -- "$shoal_cd_file"
  if [ -n "$shoal_destination" ]; then
    builtin cd -- "$shoal_destination" || return 1
  fi
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
