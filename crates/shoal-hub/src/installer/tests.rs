use std::{
    fs,
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
/// of its archive, the archive's own when none.
fn publish(root: &Path, calls: &Path, checksum: Option<&str>) -> String {
    let platform = shoal_core::update::platform().unwrap();
    let (archive, entry, program) = match cfg!(target_os = "macos") {
        true => ("-app.tar.gz", "Shoal.app", "Shoal.app/Contents/MacOS/shoal"),
        false => (".tar.gz", "shoal", "shoal"),
    };
    let archive = format!("shoal-v{VERSION}-{platform}{archive}");
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
    let packed = Command::new("tar")
        .arg("-czf")
        .arg(assets.join(&archive))
        .arg("-C")
        .arg(&build)
        .arg(entry)
        .status()
        .unwrap();
    assert!(packed.success());
    let digest = Sha256::digest(fs::read(assets.join(&archive)).unwrap());
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let sums = format!("{}  {archive}\n", checksum.unwrap_or(&actual));
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
fn the_installer_refuses_an_archive_that_does_not_match_its_checksum() {
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
