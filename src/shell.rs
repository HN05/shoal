use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

use crate::cli::{
    context::Context as CliContext,
    output::{Palette, Style},
};

pub const INIT_COMMAND: &str = "source <(shoal shell init)";

/// Best-effort detection of the documented initialization forms; never execute
/// startup files or let an unreadable one fail service installation.
pub fn init_configured(home: &Path, zsh_directory: Option<&Path>) -> bool {
    [
        home.join(".bashrc"),
        zsh_directory.unwrap_or(home).join(".zshrc"),
    ]
    .iter()
    .filter_map(|path| std::fs::read_to_string(path).ok())
    .any(|contents| contents.lines().any(is_init_line))
}

fn is_init_line(line: &str) -> bool {
    let command = line
        .split('#')
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    matches!(
        command.trim_end_matches(';').trim_end(),
        INIT_COMMAND
            | "source <(command shoal shell init)"
            | "eval \"$(shoal shell init)\""
            | "eval \"$(command shoal shell init)\""
    )
}

/// Directory changes use a private data file, never shell code evaluated from
/// a repository path or command output. The wrapper works in Bash and Zsh.
pub const INIT: &str = r#"_shoal_recover_directory() {
  local shoal_status=$? shoal_recovery
  if [ -z "${SHOAL_SCOPE_TOKEN+x}" ] && { [ ! -d "$PWD" ] || ! builtin pwd -P >/dev/null 2>&1; }; then
    shoal_recovery="$(command shoal shell recover -- "$PWD" 2>/dev/null)" &&
      builtin cd -- "$shoal_recovery"
  fi
  return "$shoal_status"
}

shoal() {
  local shoal_cd_file shoal_destination shoal_exit=0
  shoal_cd_file="$(mktemp "${TMPDIR:-/tmp}/shoal-cd.XXXXXXXX")" || return 1
  SHOAL_PREVIOUS_DIR="${OLDPWD-}" SHOAL_SHELL_DIRECTIVE="$shoal_cd_file" command shoal "$@" || shoal_exit=$?
  IFS= read -r shoal_destination < "$shoal_cd_file" || :
  command rm -f -- "$shoal_cd_file"
  if [ -n "$shoal_destination" ]; then
    if [ ! -d "$shoal_destination" ]; then
      shoal_destination="$(command shoal shell recover -- "$shoal_destination")" || return "$?"
    fi
    builtin cd -- "$shoal_destination" || return 1
  fi
  _shoal_recover_directory || :
  return "$shoal_exit"
}

if [ -n "${ZSH_VERSION-}" ]; then
  typeset -ga precmd_functions
  if (( ! ${precmd_functions[(Ie)_shoal_recover_directory]} )); then
    precmd_functions=(_shoal_recover_directory "${precmd_functions[@]}")
  fi
  if ! typeset -f compdef >/dev/null; then
    autoload -Uz compinit
    compinit
  fi
  source <(command shoal completions zsh)
elif [ -n "${BASH_VERSION-}" ]; then
  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a "* ]]; then
    case " ${PROMPT_COMMAND[*]-} " in
      *" _shoal_recover_directory "*) ;;
      *) PROMPT_COMMAND=(_shoal_recover_directory "${PROMPT_COMMAND[@]}") ;;
    esac
  else
    case "${PROMPT_COMMAND-}" in
      _shoal_recover_directory|"_shoal_recover_directory;"*) ;;
      *) PROMPT_COMMAND="_shoal_recover_directory${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
    esac
  fi
  eval "$(command shoal completions bash)"
fi
"#;

/// Cleanup may already have released the workspace record. Recover using only
/// the shell's last path, without needing a daemon or a usable current directory.
pub fn recovery_directory(path: &Path) -> Result<PathBuf> {
    ensure!(
        !crate::env::is_scoped(),
        "workspace processes cannot navigate outside their worktree"
    );
    ensure!(path.is_absolute(), "recovery path must be absolute");
    let destination = path
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .context("no surviving parent directory")?;
    let text = destination.to_str().context("recovery path is not UTF-8")?;
    ensure!(
        !text.contains(['\n', '\r']),
        "shell navigation does not support newlines in paths"
    );
    Ok(destination.to_owned())
}

pub fn completions(shell: clap_complete::Shell) -> Result<String> {
    let mut script = Vec::new();
    let shells = clap_complete::env::Shells::builtins();
    let adapter = shells
        .completer(&shell.to_string())
        .context("unsupported completion shell")?;
    let executable = crate::service::executable(None)?;
    adapter.write_registration(
        crate::env::COMPLETE,
        "shoal",
        "shoal",
        executable
            .to_str()
            .context("executable path is not UTF-8")?,
        &mut script,
    )?;
    let mut script = String::from_utf8(script).context("completion script is not UTF-8")?;
    if shell == clap_complete::Shell::Zsh {
        // clap emits targets before options. fzf-tab otherwise alphabetizes them
        // again, putting every --flag ahead of the targets.
        script.push_str("\nzstyle ':completion:*:shoal:*' sort false\n");
    }
    Ok(script)
}

pub fn navigate(path: &Path, json: bool) -> Result<()> {
    if json {
        return Ok(());
    }
    if let Some(destination) = std::env::var_os(crate::env::SHELL_DIRECTIVE) {
        let path = path
            .to_str()
            .context("shell navigation path is not UTF-8")?;
        ensure!(
            !path.contains(['\n', '\r']),
            "shell navigation does not support newlines in paths"
        );
        std::fs::write(destination, format!("{path}\n")).context("write shell directory change")?;
    } else if CliContext::is_interactive(json) {
        eprintln!(
            "{} shell integration is not loaded; run `{INIT_COMMAND}` to enable directory changes",
            Palette::stderr(json).paint(Style::Warning, "warning:")
        );
    }
    Ok(())
}

/// OLDPWD is shell-local state, so the wrapper passes it explicitly. There is no
/// daemon-wide history to leak navigation between independent terminals.
pub fn previous_directory() -> Result<PathBuf> {
    let previous = std::env::var_os(crate::env::PREVIOUS_DIR)
        .or_else(|| std::env::var_os("OLDPWD"))
        .filter(|p| !p.is_empty())
        .context("no previous directory; use the shell integration and change directory first")?;
    let previous = PathBuf::from(previous);
    ensure!(
        previous.is_absolute(),
        "previous directory must be an absolute path"
    );
    ensure!(
        previous.is_dir(),
        "previous directory no longer exists; staying in the current directory"
    );
    std::fs::canonicalize(previous).context("resolve previous directory")
}
