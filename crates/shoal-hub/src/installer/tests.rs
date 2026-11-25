use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sha2::{Digest as _, Sha256};

use super::script;
use shoal_core::test_support::{install_test_script, printed};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A release of this version for this platform under `root`, served
/// through `file://`, whose `shoal` reports the version and records every
/// other run's arguments in `calls`; `checksum` is what `SHA256SUMS` says
/// of its asset, the asset's own when none. macOS's asset is the archive
/// holding `Shoal.app`, every other platform's the bare binary.
fn publish(root: &Path, calls: &Path, checksum: Option<&str>) -> String {
    let platform = shoal_core::update::platform().unwrap();
    let bundle = cfg!(target_os = "macos");
    let (suffix, program) = match bundle {
        true => ("-app.tar.gz", "Shoal.app/Contents/MacOS/shoal"),
        false => ("", "shoal"),
    };
    let asset = format!("shoal-v{VERSION}-{platform}{suffix}");
    let build = root.join("build");
    install_test_script(
        build.join(program),
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'shoal {VERSION}'; else echo \"$*\" >> '{}'; fi\n",
            calls.display()
        ),
    );
    let assets = root.join(format!("releases/download/v{VERSION}"));
    fs::create_dir_all(&assets).unwrap();
    if bundle {
        let packed = Command::new("tar")
            .arg("-czf")
            .arg(assets.join(&asset))
            .arg("-C")
            .arg(&build)
            .arg("Shoal.app")
            .status()
            .unwrap();
        assert!(packed.success());
    } else {
        fs::copy(build.join(program), assets.join(&asset)).unwrap();
        // A download carries no mode, so the installer makes it executable.
        fs::set_permissions(assets.join(&asset), fs::Permissions::from_mode(0o644)).unwrap();
    }
    let digest = Sha256::digest(fs::read(assets.join(&asset)).unwrap());
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let sums = format!("{}  {asset}\n", checksum.unwrap_or(&actual));
    fs::write(assets.join("SHA256SUMS"), sums).unwrap();
    format!("file://{}", root.join("releases").display())
}

/// Run the installer of the releases under `releases` with `code`, as
/// `curl … | sh -s -- <code>` does, in `home`.
fn install(home: &Path, releases: &str, code: &str) -> Output {
    let path = std::env::var_os("PATH").unwrap_or_default();
    Command::new("sh")
        .args(["-c", &script(releases), "sh", code])
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn installed(home: &Path) -> PathBuf {
    match cfg!(target_os = "macos") {
        true => home.join("Applications/Shoal.app/Contents/MacOS/shoal"),
        false => home.join(".local/bin/shoal"),
    }
}

#[test]
fn the_installer_installs_the_hubs_version_and_joins_with_the_code() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let calls = root.path().join("calls");
    let releases = publish(root.path(), &calls, None);
    let code = "http://hub:7474/pair#code=ABC&key=def";
    let output = install(&home, &releases, code);
    assert!(output.status.success(), "{}", printed(&output));
    let reported = Command::new(installed(&home))
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(reported.stdout).unwrap().trim(),
        format!("shoal {VERSION}")
    );
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        format!("daemon install\nhub join {code}\n")
    );
}

#[test]
fn the_installer_keeps_an_installation_it_finds() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let calls = root.path().join("calls");
    let releases = publish(root.path(), &calls, None);
    let existing = home.join(".local/bin/shoal");
    fs::create_dir_all(existing.parent().unwrap()).unwrap();
    fs::write(&existing, "mine").unwrap();
    let output = install(&home, &releases, "code");
    assert!(!output.status.success(), "{}", printed(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("shoal hub join"));
    assert_eq!(fs::read_to_string(&existing).unwrap(), "mine");
    assert!(!calls.exists());
}

#[test]
fn the_installer_refuses_an_asset_that_does_not_match_its_checksum() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let calls = root.path().join("calls");
    let releases = publish(root.path(), &calls, Some(&"0".repeat(64)));
    let output = install(&home, &releases, "code");
    assert!(!output.status.success(), "{}", printed(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("checksum"));
    assert!(!installed(&home).exists());
    assert!(!calls.exists());
}
