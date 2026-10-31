use super::*;
use crate::test_support::install_test_script;

const CHECKSUMS: &str = "\
486f  shoal-v0.9.6-linux-arm64.tar.gz
3a79  shoal-v0.9.6-macos-arm64.tar.gz
";

#[test]
fn checksums_name_the_platform_archive_and_version() -> Result<()> {
    let release = Release::from_checksums("https://host/r", "macos-arm64", CHECKSUMS)?;
    assert_eq!(
        release,
        Release {
            base: "https://host/r".to_owned(),
            version: "0.9.6".to_owned(),
            archive: "shoal-v0.9.6-macos-arm64.tar.gz".to_owned(),
            sha256: "3a79".to_owned(),
        }
    );
    let missing = Release::from_checksums("https://host/r", "linux-x86_64", CHECKSUMS);
    assert!(missing.is_err());
    Ok(())
}

#[test]
fn only_a_newer_release_updates() -> Result<()> {
    assert!(newer("0.10.0", "0.9.5")?);
    assert!(newer("1.0.0", "0.99.99")?);
    assert!(!newer("0.9.5", "0.9.5")?);
    assert!(!newer("0.9.4", "0.9.5")?);
    assert!(newer("0.9", "0.9.5").is_err());
    Ok(())
}

#[test]
fn homebrew_kegs_are_refused() {
    assert!(homebrew_installed(Path::new(
        "/opt/homebrew/Cellar/shoal/0.9.5/libexec/shoal"
    )));
    assert!(!homebrew_installed(Path::new("/Users/me/.local/bin/shoal")));
}

#[test]
fn an_app_bundle_executable_updates_the_whole_bundle() {
    let binary = Path::new("/Users/me/Applications/Shoal.app/Contents/MacOS/shoal");
    let installation = Installation::of(binary);
    assert_eq!(
        installation,
        Installation::Bundle(PathBuf::from("/Users/me/Applications/Shoal.app"))
    );
    assert_eq!(installation.entry(), "Shoal.app");
    assert_eq!(
        installation.executable(Path::new("/s/Shoal.app")),
        Path::new("/s/Shoal.app/Contents/MacOS/shoal")
    );
    let plain = Installation::of(Path::new("/Users/me/.local/bin/shoal"));
    assert_eq!(plain.entry(), "shoal");
}

/// A release directory served through `file://` whose archive holds `entry`,
/// with the executable `program` under it reporting `version`.
fn publish(
    root: &Path,
    version: &str,
    platform: &str,
    entry: &str,
    program: &str,
) -> Result<String> {
    let build = root.join("build");
    let script = build.join(program);
    fs::create_dir_all(script.parent().context("parent")?)?;
    install_test_script(&script, &format!("#!/bin/sh\necho 'shoal {version}'\n"));
    let archive = format!("shoal-v{version}-{platform}.tar.gz");
    let assets = root.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&assets)?;
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(assets.join(&archive))
        .arg("-C")
        .arg(&build)
        .arg(entry)
        .status()?;
    ensure!(status.success(), "tar failed");
    let digest = Sha256::digest(fs::read(assets.join(&archive))?);
    let sha256: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let latest = root.join("releases/latest/download");
    fs::create_dir_all(&latest)?;
    fs::write(latest.join("SHA256SUMS"), format!("{sha256}  {archive}\n"))?;
    Ok(format!("file://{}", root.join("releases").display()))
}

#[tokio::test]
async fn install_replaces_the_binary_with_the_verified_release() -> Result<()> {
    let root = tempfile::tempdir()?;
    let base = publish(root.path(), "9.9.9", "linux-arm64", "shoal", "shoal")?;
    let target = root.path().join("bin/shoal");
    fs::create_dir_all(target.parent().context("parent")?)?;
    fs::write(&target, "old")?;
    let release = Release::latest(&base, "linux-arm64").await?;
    release.install(&Installation::of(&target)).await?;
    let output = std::process::Command::new(&target)
        .arg("--version")
        .output()?;
    assert_eq!(String::from_utf8(output.stdout)?, "shoal 9.9.9\n");
    // The staging directory is gone.
    assert_eq!(fs::read_dir(root.path().join("bin"))?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn install_refuses_an_archive_that_does_not_match_its_checksum() -> Result<()> {
    let root = tempfile::tempdir()?;
    let base = publish(root.path(), "9.9.9", "linux-arm64", "shoal", "shoal")?;
    let target = root.path().join("bin/shoal");
    fs::create_dir_all(target.parent().context("parent")?)?;
    fs::write(&target, "old")?;
    let mut release = Release::latest(&base, "linux-arm64").await?;
    release.sha256 = "0".repeat(64);
    let error = release
        .install(&Installation::of(&target))
        .await
        .err()
        .context("installed")?;
    assert!(format!("{error:#}").contains("checksum"), "{error:#}");
    assert_eq!(fs::read_to_string(&target)?, "old");
    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn install_swaps_in_the_verified_app_bundle() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executable = "Contents/MacOS/shoal";
    let base = publish(
        root.path(),
        "9.9.9",
        "macos-arm64",
        "Shoal.app",
        "Shoal.app/Contents/MacOS/shoal",
    )?;
    let apps = root.path().join("apps");
    let bundle = apps.join("Shoal.app");
    fs::create_dir_all(bundle.join("Contents/MacOS"))?;
    fs::write(bundle.join(executable), "old")?;
    fs::write(bundle.join("Contents/stale"), "old")?;
    let release = Release::latest(&base, "macos-arm64").await?;
    release
        .install(&Installation::of(&bundle.join(executable)))
        .await?;
    let output = std::process::Command::new(bundle.join(executable))
        .arg("--version")
        .output()?;
    assert_eq!(String::from_utf8(output.stdout)?, "shoal 9.9.9\n");
    // The old bundle went with the staging directory.
    assert!(!bundle.join("Contents/stale").exists());
    assert_eq!(fs::read_dir(&apps)?.count(), 1);
    Ok(())
}
