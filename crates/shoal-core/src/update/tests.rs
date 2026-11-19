use super::*;
use crate::test_support::install_test_script;

const CHECKSUMS: &str = "\
486f  shoal-v0.9.6-linux-arm64.tar.gz
9c1e  shoal-v0.9.6-macos-arm64-app.tar.gz
3a79  shoal-v0.9.6-macos-arm64.tar.gz
";

#[test]
fn checksums_name_the_installation_archive_and_version() -> Result<()> {
    let binary = Installation::of(Path::new("/Users/me/.local/bin/shoal"));
    let suffix = binary.archive_suffix("macos-arm64");
    let release = Release::from_checksums("https://host/r", &suffix, CHECKSUMS)?;
    assert_eq!(
        release,
        Release {
            base: "https://host/r".to_owned(),
            version: "0.9.6".to_owned(),
            archive: "shoal-v0.9.6-macos-arm64.tar.gz".to_owned(),
            sha256: "3a79".to_owned(),
        }
    );
    let bundle = Installation::of(Path::new(
        "/Users/me/Applications/Shoal.app/Contents/MacOS/shoal",
    ));
    let suffix = bundle.archive_suffix("macos-arm64");
    let release = Release::from_checksums("https://host/r", &suffix, CHECKSUMS)?;
    assert_eq!(release.archive, "shoal-v0.9.6-macos-arm64-app.tar.gz");
    assert_eq!(release.sha256, "9c1e");
    let missing = Release::from_checksums(
        "https://host/r",
        &bundle.archive_suffix("linux-arm64"),
        CHECKSUMS,
    );
    assert!(missing.is_err());
    Ok(())
}

#[test]
fn only_a_newer_latest_release_updates() -> Result<()> {
    let newer = |release, current| Target::Latest.replaces(release, current);
    assert!(newer("0.10.0", "0.9.5")?);
    assert!(newer("1.0.0", "0.99.99")?);
    assert!(!newer("0.9.5", "0.9.5")?);
    assert!(!newer("0.9.4", "0.9.5")?);
    assert!(newer("0.9", "0.9.5").is_err());
    Ok(())
}

#[test]
fn an_exact_release_updates_unless_it_runs_already() -> Result<()> {
    let exact = Target::Exact("0.9.4".into());
    assert!(exact.replaces("0.9.4", "0.9.5")?);
    assert!(!exact.replaces("0.9.4", "0.9.4")?);
    Ok(())
}

#[test]
fn homebrew_kegs_are_refused() {
    let keg = Installed::of(Path::new("/opt/homebrew/Cellar/shoal/0.9.5/libexec/shoal"));
    assert_eq!(keg, Installed::Homebrew);
    assert!(keg.downloaded().is_err());
    let downloaded = Installed::of(Path::new("/Users/me/.local/bin/shoal"));
    assert!(downloaded.downloaded().is_ok());
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

/// A release directory served through `file://` whose archive, named with
/// `suffix`, holds `entry`, with the executable `program` under it reporting
/// `version`.
fn publish(root: &Path, version: &str, suffix: &str, entry: &str, program: &str) -> Result<String> {
    let build = root.join("build");
    let script = build.join(program);
    fs::create_dir_all(script.parent().context("parent")?)?;
    install_test_script(&script, format!("#!/bin/sh\necho 'shoal {version}'\n"));
    let archive = format!("shoal-v{version}{suffix}");
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
    let checksums = format!("{sha256}  {archive}\n");
    fs::write(assets.join("SHA256SUMS"), &checksums)?;
    let latest = root.join("releases/latest/download");
    fs::create_dir_all(&latest)?;
    fs::write(latest.join("SHA256SUMS"), checksums)?;
    Ok(format!("file://{}", root.join("releases").display()))
}

const SUFFIX: &str = "-linux-arm64.tar.gz";

#[tokio::test]
async fn install_replaces_the_binary_with_the_verified_release() -> Result<()> {
    let root = tempfile::tempdir()?;
    let base = publish(root.path(), "9.9.9", SUFFIX, "shoal", "shoal")?;
    let target = root.path().join("bin/shoal");
    fs::create_dir_all(target.parent().context("parent")?)?;
    fs::write(&target, "old")?;
    let release = Release::find(&base, &Target::Latest, SUFFIX).await?;
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
    let base = publish(root.path(), "9.9.9", SUFFIX, "shoal", "shoal")?;
    let target = root.path().join("bin/shoal");
    fs::create_dir_all(target.parent().context("parent")?)?;
    fs::write(&target, "old")?;
    let mut release = Release::find(&base, &Target::Latest, SUFFIX).await?;
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
    let apps = root.path().join("apps");
    let bundle = apps.join("Shoal.app");
    let installation = Installation::of(&bundle.join(executable));
    let suffix = installation.archive_suffix("macos-arm64");
    let base = publish(
        root.path(),
        "9.9.9",
        &suffix,
        "Shoal.app",
        "Shoal.app/Contents/MacOS/shoal",
    )?;
    fs::create_dir_all(bundle.join("Contents/MacOS"))?;
    fs::write(bundle.join(executable), "old")?;
    fs::write(bundle.join("Contents/stale"), "old")?;
    let release = Release::find(&base, &Target::Latest, &suffix).await?;
    release.install(&installation).await?;
    let output = std::process::Command::new(bundle.join(executable))
        .arg("--version")
        .output()?;
    assert_eq!(String::from_utf8(output.stdout)?, "shoal 9.9.9\n");
    // The old bundle went with the staging directory.
    assert!(!bundle.join("Contents/stale").exists());
    assert_eq!(fs::read_dir(&apps)?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn an_exact_release_installs_from_its_own_checksums() -> Result<()> {
    let root = tempfile::tempdir()?;
    publish(root.path(), "0.1.0", SUFFIX, "shoal", "shoal")?;
    let base = publish(root.path(), "9.9.9", SUFFIX, "shoal", "shoal")?;
    let target = root.path().join("bin/shoal");
    fs::create_dir_all(target.parent().context("parent")?)?;
    fs::write(&target, "old")?;
    let release = Release::find(&base, &Target::Exact("0.1.0".into()), SUFFIX).await?;
    release.install(&Installation::of(&target)).await?;
    let output = std::process::Command::new(&target)
        .arg("--version")
        .output()?;
    assert_eq!(String::from_utf8(output.stdout)?, "shoal 0.1.0\n");
    let missing = Release::find(&base, &Target::Exact("0.2.0".into()), SUFFIX).await;
    assert!(missing.is_err());
    Ok(())
}
