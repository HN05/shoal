use super::*;
use crate::test_support::install_test_script;

const CHECKSUMS: &str = "\
486f  shoal-v0.9.6-linux-arm64
9c1e  shoal-v0.9.6-macos-arm64-app.tar.gz
3a79  shoal-v0.9.6-macos-arm64
";

#[test]
fn checksums_name_the_installation_asset_and_version() -> Result<()> {
    let binary = Installation::of(Path::new("/Users/me/.local/bin/shoal"));
    let suffix = binary.asset_suffix("macos-arm64");
    let release = Release::from_checksums("https://host/r", &suffix, CHECKSUMS)?;
    assert_eq!(
        release,
        Release {
            base: "https://host/r".to_owned(),
            version: "0.9.6".to_owned(),
            asset: "shoal-v0.9.6-macos-arm64".to_owned(),
            sha256: "3a79".to_owned(),
        }
    );
    let bundle = Installation::of(Path::new(
        "/Users/me/Applications/Shoal.app/Contents/MacOS/shoal",
    ));
    let suffix = bundle.asset_suffix("macos-arm64");
    let release = Release::from_checksums("https://host/r", &suffix, CHECKSUMS)?;
    assert_eq!(release.asset, "shoal-v0.9.6-macos-arm64-app.tar.gz");
    assert_eq!(release.sha256, "9c1e");
    let missing = Release::from_checksums(
        "https://host/r",
        &bundle.asset_suffix("linux-arm64"),
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
    assert_eq!(
        installation.executable(Path::new("/s/Shoal.app")),
        Path::new("/s/Shoal.app/Contents/MacOS/shoal")
    );
}

/// A release directory served through `file://` whose asset, named with
/// `suffix`, is a bare binary reporting `version`, or with `bundle` an
/// archive holding `Shoal.app` with that binary.
fn publish(root: &Path, version: &str, suffix: &str, bundle: bool) -> Result<String> {
    let build = root.join("build");
    let script = match bundle {
        true => build.join("Shoal.app/Contents/MacOS/shoal"),
        false => build.join("shoal"),
    };
    fs::create_dir_all(script.parent().context("parent")?)?;
    install_test_script(&script, format!("#!/bin/sh\necho 'shoal {version}'\n"));
    let asset = format!("shoal-v{version}{suffix}");
    let assets = root.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&assets)?;
    if bundle {
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(assets.join(&asset))
            .arg("-C")
            .arg(&build)
            .arg("Shoal.app")
            .status()?;
        ensure!(status.success(), "tar failed");
    } else {
        fs::copy(&script, assets.join(&asset))?;
        // A download carries no mode, so the update makes it executable.
        fs::set_permissions(assets.join(&asset), fs::Permissions::from_mode(0o644))?;
    }
    let digest = Sha256::digest(fs::read(assets.join(&asset))?);
    let sha256: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    let checksums = format!("{sha256}  {asset}\n");
    fs::write(assets.join("SHA256SUMS"), &checksums)?;
    let latest = root.join("releases/latest/download");
    fs::create_dir_all(&latest)?;
    fs::write(latest.join("SHA256SUMS"), checksums)?;
    Ok(format!("file://{}", root.join("releases").display()))
}

const SUFFIX: &str = "-linux-arm64";

#[tokio::test]
async fn install_replaces_the_binary_with_the_verified_release() -> Result<()> {
    let root = tempfile::tempdir()?;
    let base = publish(root.path(), "9.9.9", SUFFIX, false)?;
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
async fn install_refuses_an_asset_that_does_not_match_its_checksum() -> Result<()> {
    let root = tempfile::tempdir()?;
    let base = publish(root.path(), "9.9.9", SUFFIX, false)?;
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
    let suffix = installation.asset_suffix("macos-arm64");
    let base = publish(root.path(), "9.9.9", &suffix, true)?;
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
    publish(root.path(), "0.1.0", SUFFIX, false)?;
    let base = publish(root.path(), "9.9.9", SUFFIX, false)?;
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
