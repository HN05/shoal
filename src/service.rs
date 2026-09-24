use crate::fsutil::{self, Permissions, ReplaceOptions};
use crate::tools::Tool;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use tokio::process::Command;

use crate::paths::Paths;

const LABEL: &str = "com.shoal.daemon";
const UNIT: &str = "shoal.service";

#[derive(Clone, Copy)]
pub enum Platform {
    Mac,
    Linux,
}

impl Platform {
    pub fn current() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Self::Mac)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else {
            bail!("Shoal supports macOS and Linux")
        }
    }
}

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
        None => {
            let arg = PathBuf::from(
                std::env::args_os()
                    .next()
                    .context("missing executable path")?,
            );
            if arg.components().count() > 1 {
                arg
            } else {
                crate::fsutil::find_executable(
                    arg.as_os_str(),
                    &std::env::var_os("PATH").unwrap_or_default(),
                )
                .unwrap_or(std::env::current_exe()?)
            }
        }
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

fn text(path: &Path) -> Result<&str> {
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

async fn command(tool: Tool, args: &[&str], check: bool) -> Result<bool> {
    let program = tool.program();
    let mut command = Command::new(program);
    command.args(args);
    let output = crate::subprocess::Run::new(command)
        .timeout(Duration::from_secs(20))
        .capture()
        .await
        .with_context(|| format!("run {program}; a working per-user service manager is required (use `shoal daemon run` otherwise)"))?;
    if check && !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            crate::subprocess::diagnostic(&output.stderr).trim()
        );
    }
    Ok(output.status.success())
}

fn domain() -> String {
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    format!("gui/{}", unsafe { libc::geteuid() })
}

pub async fn setup(paths: &Paths, executable: &Path, preserve_running: bool) -> Result<()> {
    let platform = Platform::current()?;
    let definition = definition(paths, executable, platform)?;
    paths.prepare()?;
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
    match platform {
        Platform::Mac => {
            let target = format!("{}/{LABEL}", domain());
            if changed
                && !preserve_running
                && command(Tool::Launchctl, &["print", &target], false).await?
            {
                command(Tool::Launchctl, &["bootout", &target], true).await?;
            }
        }
        Platform::Linux => {
            command(Tool::Systemctl, &["--user", "daemon-reload"], true).await?;
            command(Tool::Systemctl, &["--user", "enable", UNIT], true).await?;
            if changed && !preserve_running {
                command(Tool::Systemctl, &["--user", "restart", UNIT], true).await?;
            }
        }
    }
    start(paths).await
}

pub async fn start(paths: &Paths) -> Result<()> {
    let platform = Platform::current()?;
    let path = file(paths, platform);
    ensure!(
        path.is_file(),
        "daemon service is not installed; run `shoal install`"
    );
    check_state_dir(paths, &path, platform)?;
    match platform {
        Platform::Mac => {
            let domain = domain();
            let target = format!("{domain}/{LABEL}");
            command(Tool::Launchctl, &["enable", &target], true).await?;
            if !command(Tool::Launchctl, &["print", &target], false).await? {
                command(Tool::Launchctl, &["bootstrap", &domain, text(&path)?], true).await?;
            }
            command(Tool::Launchctl, &["kickstart", &target], true).await?;
        }
        Platform::Linux => {
            command(Tool::Systemctl, &["--user", "start", UNIT], true).await?;
        }
    }
    Ok(())
}

pub async fn stop(paths: &Paths) -> Result<()> {
    let platform = Platform::current()?;
    let path = file(paths, platform);
    if !path.exists() {
        return Ok(());
    }
    check_state_dir(paths, &path, platform)?;
    match platform {
        Platform::Mac => {
            let target = format!("{}/{LABEL}", domain());
            if command(Tool::Launchctl, &["print", &target], false).await? {
                command(Tool::Launchctl, &["bootout", &target], true).await?;
            }
        }
        Platform::Linux => {
            if file(paths, Platform::Linux).exists() {
                command(Tool::Systemctl, &["--user", "stop", UNIT], true).await?;
            }
        }
    }
    Ok(())
}

fn check_state_dir(paths: &Paths, path: &Path, platform: Platform) -> Result<()> {
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
        "installed service uses another state directory; use its --state-dir or rerun `shoal install`"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths::for_test("/tmp/a & b/%name/$data")
    }

    #[test]
    fn launch_agent_preserves_literal_arguments() {
        let definition =
            definition(&paths(), Path::new("/tmp/a & b/shoal"), Platform::Mac).unwrap();
        let value = plist::Value::from_reader_xml(definition.content.as_bytes()).unwrap();
        let args = value.as_dictionary().unwrap()["ProgramArguments"]
            .as_array()
            .unwrap();
        assert_eq!(args[0].as_string(), Some("/tmp/a & b/shoal"));
        assert_eq!(args[2].as_string(), Some("/tmp/a & b/%name/$data/state"));
    }

    #[test]
    fn systemd_escapes_expansion_and_quotes() {
        assert_eq!(
            systemd_quote("/a %n/$HOME/\"b", true).unwrap(),
            "\"/a %%n/$$HOME/\\\"b\""
        );
        assert!(systemd_quote("/a\nExecStart=bad", true).is_err());
        let definition = definition(&paths(), Path::new("/tmp/shoal"), Platform::Linux).unwrap();
        assert!(definition.content.contains("Type=exec"));
        assert!(definition.content.contains("%%name/$$data"));
    }
}
