//! Service definition paths, rendering and ownership checks.
use super::{LABEL, Platform, UNIT, control::SERVICE_STOP_TIMEOUT};
use crate::paths::StateFiles;
use crate::{
    fsutil::{
        self,
        atomic::{Permissions, ReplaceOptions},
    },
    paths::Paths,
};
use anyhow::{Context, Result, bail, ensure};
use shoal_protocol::service::Definition;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn file(paths: &Paths, platform: Platform) -> PathBuf {
    match platform {
        Platform::Mac => paths
            .home
            .join(format!("Library/LaunchAgents/{LABEL}.plist")),
        Platform::Linux => std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| paths.home.join(".config"))
            .join("systemd/user")
            .join(UNIT),
    }
}

pub fn executable(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let path = match explicit {
        Some(path) => path,
        None => fsutil::executables::invoked_executable()?,
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        crate::fsutil::executables::is_executable(&path).unwrap_or(false),
        "not an executable file: {}",
        path.display()
    );
    // Preserve an installation symlink rather than resolving into a versioned keg.
    Ok(path)
}

pub(super) fn text(path: &Path) -> Result<&str> {
    path.to_str().context("service paths must be valid UTF-8")
}

fn xml(value: &str) -> Result<String> {
    ensure!(
        !value.chars().any(char::is_control),
        "service values cannot contain control characters"
    );
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn systemd_quote(value: &str, exec: bool) -> Result<String> {
    ensure!(
        !value.chars().any(char::is_control),
        "service values cannot contain control characters"
    );
    let value = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    let value = if exec {
        value.replace('$', "$$")
    } else {
        value
    };
    Ok(format!("\"{value}\""))
}

/// What a service definition starts: the daemon's executable and the
/// `PATH` it gives it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Launch {
    pub(super) executable: PathBuf,
    pub(super) search_path: String,
}

/// The definition that runs `executable` with the `PATH` this process has.
pub fn definition(paths: &Paths, executable: &Path, platform: Platform) -> Result<Definition> {
    let launch = Launch {
        executable: executable.to_owned(),
        search_path: std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
    };
    render(paths, &launch, platform)
}

/// The definition at `paths`' service file that starts `launch`.
pub(super) fn render(paths: &Paths, launch: &Launch, platform: Platform) -> Result<Definition> {
    let search_path = &launch.search_path;
    let args = [
        text(&launch.executable)?,
        "--state-dir",
        text(&paths.state)?,
        "daemon",
        "run",
        "--managed",
    ];
    let stop_seconds = SERVICE_STOP_TIMEOUT.as_secs();
    let content = match platform {
        Platform::Mac => {
            let args = args
                .iter()
                .map(|a| xml(a).map(|a| format!("<string>{a}</string>")))
                .collect::<Result<Vec<_>>>()?
                .join("\n");
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>{args}</array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ThrottleInterval</key><integer>5</integer>\n<key>ExitTimeOut</key><integer>{stop_seconds}</integer>\n<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
                xml(search_path)?,
                xml(text(&paths.daemon_log())?)?
            )
        }
        Platform::Linux => {
            let args = args
                .iter()
                .map(|a| systemd_quote(a, true))
                .collect::<Result<Vec<_>>>()?
                .join(" ");
            format!(
                "[Unit]\nDescription=Shoal local agent resource manager\n\n[Service]\nType=exec\nExecStart={args}\nEnvironment={}\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec={stop_seconds}\nOOMPolicy=continue\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
                systemd_quote(&format!("PATH={search_path}"), false)?
            )
        }
    };
    Ok(Definition {
        path: file(paths, platform),
        content,
    })
}

/// What the service definition `content` starts, as [`render`] wrote it.
pub(super) fn launch(content: &str, platform: Platform) -> Result<Launch> {
    match platform {
        Platform::Mac => plist_launch(content),
        Platform::Linux => unit_launch(content),
    }
}

fn plist_launch(content: &str) -> Result<Launch> {
    let value = plist::Value::from_reader_xml(content.as_bytes())?;
    let job = value.as_dictionary().context("the plist holds no job")?;
    let executable = (job.get("ProgramArguments"))
        .and_then(plist::Value::as_array)
        .and_then(|args| args.first())
        .and_then(plist::Value::as_string)
        .context("the plist names no program")?;
    let search_path = (job.get("EnvironmentVariables"))
        .and_then(plist::Value::as_dictionary)
        .and_then(|environment| environment.get("PATH"))
        .and_then(plist::Value::as_string)
        .context("the plist sets no PATH")?;
    Ok(Launch {
        executable: executable.into(),
        search_path: search_path.to_owned(),
    })
}

fn unit_launch(content: &str) -> Result<Launch> {
    let setting = |key: &str| {
        content
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .with_context(|| format!("the unit sets no {key}"))
    };
    let executable = systemd_unquote(setting("ExecStart")?, true)?;
    let environment = systemd_unquote(setting("Environment")?, false)?;
    let search_path =
        (environment.strip_prefix("PATH=")).context("the unit's environment sets no PATH")?;
    Ok(Launch {
        executable: executable.into(),
        search_path: search_path.to_owned(),
    })
}

/// The first value of `line`, quoted as [`systemd_quote`] quotes it.
fn systemd_unquote(line: &str, exec: bool) -> Result<String> {
    let mut chars = line
        .strip_prefix('"')
        .context("unquoted service value")?
        .chars();
    let mut value = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(value),
            '\\' => value.extend(chars.next()),
            '%' => value.extend(chars.next().filter(|&c| c == '%')),
            '$' if exec => value.extend(chars.next().filter(|&c| c == '$')),
            c => value.push(c),
        }
    }
    bail!("unterminated service value")
}

pub(super) fn check_state_dir(paths: &Paths, path: &Path, platform: Platform) -> Result<()> {
    ensure!(
        state_dir_matches(paths, path, platform)?,
        "installed service uses another state directory; use its --state-dir or rerun `shoal daemon install`"
    );
    Ok(())
}

pub(super) fn state_dir_matches(paths: &Paths, path: &Path, platform: Platform) -> Result<bool> {
    names_state_dir(paths, &fs::read_to_string(path)?, platform)
}

/// Whether the service definition `content` runs the daemon of `paths`'
/// state directory.
pub(super) fn names_state_dir(paths: &Paths, content: &str, platform: Platform) -> Result<bool> {
    let expected = match platform {
        Platform::Mac => format!(
            "<string>--state-dir</string>\n<string>{}</string>",
            xml(text(&paths.state)?)?
        ),
        Platform::Linux => format!(
            "\"--state-dir\" {}",
            systemd_quote(text(&paths.state)?, true)?
        ),
    };
    Ok(content.contains(&expected))
}

pub(super) fn write_definition(definition: &Definition) -> Result<bool> {
    let changed = fs::read_to_string(&definition.path).ok().as_deref() != Some(&definition.content);
    if changed {
        fs::create_dir_all(
            definition
                .path
                .parent()
                .context("missing service directory")?,
        )?;
        fsutil::atomic::replace_atomically(
            &definition.path,
            definition.content.as_bytes(),
            ReplaceOptions {
                permissions: Permissions::Temporary,
                sync: true,
            },
        )
        .context("write service definition")?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
