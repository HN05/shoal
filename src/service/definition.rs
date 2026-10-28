//! Service definition paths, rendering and ownership checks.
use super::{LABEL, Platform, UNIT};
use crate::{
    fsutil::{self, Permissions, ReplaceOptions},
    paths::Paths,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub struct Definition {
    pub path: PathBuf,
    pub content: String,
}

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
        None => fsutil::invoked_executable()?,
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        crate::fsutil::is_executable(&path).unwrap_or(false),
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

pub fn definition(paths: &Paths, executable: &Path, platform: Platform) -> Result<Definition> {
    let args = [
        text(executable)?,
        "--state-dir",
        text(&paths.state)?,
        "daemon",
        "run",
        "--managed",
    ];
    let search_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let content = match platform {
        Platform::Mac => {
            let args = args
                .iter()
                .map(|a| xml(a).map(|a| format!("<string>{a}</string>")))
                .collect::<Result<Vec<_>>>()?
                .join("\n");
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>{args}</array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ThrottleInterval</key><integer>5</integer>\n<key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n",
                xml(&search_path)?,
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
                "[Unit]\nDescription=Shoal local agent resource manager\n\n[Service]\nType=exec\nExecStart={args}\nEnvironment={}\nRestart=on-failure\nRestartSec=5\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
                systemd_quote(&format!("PATH={search_path}"), false)?
            )
        }
    };
    Ok(Definition {
        path: file(paths, platform),
        content,
    })
}

pub(super) fn check_state_dir(paths: &Paths, path: &Path, platform: Platform) -> Result<()> {
    let content = fs::read_to_string(path)?;
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
    ensure!(
        content.contains(&expected),
        "installed service uses another state directory; use its --state-dir or rerun `shoal daemon install`"
    );
    Ok(())
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
        fsutil::replace_atomically(
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
