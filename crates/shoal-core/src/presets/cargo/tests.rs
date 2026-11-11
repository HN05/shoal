use std::{fs, process::Command};

use super::*;

/// Which `-Z` options the stand-in rustc takes.
#[derive(Clone, Copy)]
enum Rustc {
    /// Only for the crates `RUSTC_BOOTSTRAP` names.
    Stable,
    /// Unless `RUSTC_BOOTSTRAP=-1`.
    Nightly,
    /// None.
    WithoutFlag,
}

/// Runs the prepared wrapper in a workspace root, around a stand-in rustc
/// that prints its arguments and `RUSTC_BOOTSTRAP`, and takes `-Z` options
/// as `rustc` does. `{root}` in `args` stands for the root's path.
fn run(bootstrap: Option<&str>, compiler: Rustc, args: &[&str]) -> String {
    let args: Vec<_> = ["{root}/rustc"].iter().chain(args).copied().collect();
    run_wrapper(WRAPPER, bootstrap, compiler, &args)
}

/// Runs the prepared `wrapper` in a workspace root with `args`, where
/// `{root}/rustc` and `{root}/clippy-driver` are the same stand-in rustc.
/// `{root}` in the output also stands for the root's path.
fn run_wrapper(wrapper: &str, bootstrap: Option<&str>, compiler: Rustc, args: &[&str]) -> String {
    let kind = match compiler {
        Rustc::Stable => "stable",
        Rustc::Nightly => "nightly",
        Rustc::WithoutFlag => "without",
    };
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    prepare(root.path()).unwrap();
    let rustc = root.path().join("rustc");
    write_if_changed(
        &rustc,
        &format!(
            r#"#!/bin/sh
crate= next=
for argument in "$@"; do
  if [ -n "$next" ]; then crate=$argument; break; fi
  if [ "$argument" = --crate-name ]; then next=1; fi
done
for argument in "$@"; do
  case $argument in -Z*)
    case {kind},${{RUSTC_BOOTSTRAP-}}, in
      without,*|*,-1,) exit 1 ;;
      nightly,*|stable,1,|stable,"$crate",*|stable,*,"$crate",*) ;;
      *) exit 1 ;;
    esac ;;
  esac
done
case " $* " in *' --print=sysroot '*) exit 0 ;; esac
echo "RUSTC_BOOTSTRAP=${{RUSTC_BOOTSTRAP-unset}}"
printf '%s\n' "$@"
"#
        ),
        0o755,
    )
    .unwrap();
    fs::copy(&rustc, root.path().join("clippy-driver")).unwrap();
    let mut command = Command::new("sh");
    command
        .arg(root.path().join(wrapper))
        .args(
            args.iter()
                .map(|arg| arg.replace("{root}", &root_path.display().to_string())),
        )
        .current_dir(&root_path)
        .env_remove("RUSTC_BOOTSTRAP");
    if let Some(value) = bootstrap {
        command.env("RUSTC_BOOTSTRAP", value);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout)
        .unwrap()
        .replace(&root_path.display().to_string(), "{root}")
}

const CRATE: &[&str] = &["--crate-name", "demo", "src/lib.rs"];

#[test]
fn the_wrapper_maps_the_working_directory_for_the_crate_it_builds() {
    let mapped = "--crate-name\ndemo\nsrc/lib.rs\n-Zremap-cwd-prefix=.\n";
    assert_eq!(
        run(None, Rustc::Stable, CRATE),
        format!("RUSTC_BOOTSTRAP=demo\n{mapped}-Zallow-features=\n"),
        "the crate the wrapper names gets the flag and no unstable features"
    );
    assert_eq!(
        run(Some("other"), Rustc::Stable, CRATE),
        format!("RUSTC_BOOTSTRAP=other,demo\n{mapped}-Zallow-features=\n")
    );
    assert_eq!(
        run(Some("1"), Rustc::Stable, CRATE),
        format!("RUSTC_BOOTSTRAP=1\n{mapped}"),
        "a crate already granted keeps its features"
    );
    assert_eq!(
        run(Some("other,demo"), Rustc::Stable, CRATE),
        format!("RUSTC_BOOTSTRAP=other,demo\n{mapped}")
    );
    assert_eq!(
        run(None, Rustc::Nightly, CRATE),
        format!("RUSTC_BOOTSTRAP=unset\n{mapped}"),
        "a rustc that takes the flag keeps its features"
    );
}

#[test]
fn the_wrapper_passes_the_output_directory_relative_to_the_workspace() {
    let built = |out_dir: &str| {
        run(
            None,
            Rustc::Stable,
            &["--crate-name", "demo", "--out-dir", out_dir],
        )
        .lines()
        .nth(4)
        .unwrap()
        .to_owned()
    };
    assert_eq!(built("{root}/target/debug/deps"), "target/debug/deps");
    assert_eq!(
        built("/elsewhere/target/debug/deps"),
        "/elsewhere/target/debug/deps"
    );
}

#[test]
fn the_wrapper_passes_through_what_it_cannot_map() {
    let unchanged = "--crate-name\ndemo\nsrc/lib.rs\n";
    assert_eq!(
        run(Some("-1"), Rustc::Stable, CRATE),
        format!("RUSTC_BOOTSTRAP=-1\n{unchanged}"),
        "a refusal of unstable flags wins"
    );
    assert_eq!(
        run(None, Rustc::WithoutFlag, CRATE),
        format!("RUSTC_BOOTSTRAP=unset\n{unchanged}"),
        "a rustc without the flag builds as usual"
    );
    assert_eq!(
        run(Some("other"), Rustc::WithoutFlag, CRATE),
        format!("RUSTC_BOOTSTRAP=other\n{unchanged}"),
        "and keeps the RUSTC_BOOTSTRAP it was given"
    );
    assert_eq!(
        run(None, Rustc::Stable, &["-vV"]),
        "RUSTC_BOOTSTRAP=unset\n-vV\n"
    );
    assert_eq!(
        run(
            None,
            Rustc::Stable,
            &["-", "--crate-name", "___", "--print=cfg"]
        ),
        "RUSTC_BOOTSTRAP=unset\n-\n--crate-name\n___\n--print=cfg\n",
        "target queries see a stable compiler"
    );
}

#[test]
fn the_clippy_wrapper_maps_clippy_runs_of_the_crate_it_builds() {
    assert_eq!(
        run_wrapper(
            CLIPPY_WRAPPER,
            None,
            Rustc::Stable,
            &[
                "{root}/clippy-driver",
                "{root}/rustc",
                "--crate-name",
                "demo"
            ]
        ),
        "RUSTC_BOOTSTRAP=demo\n{root}/rustc\n--crate-name\ndemo\n\
         -Zremap-cwd-prefix=.\n-Zallow-features=\n"
    );
}

#[test]
fn the_clippy_wrapper_passes_other_runs_through() {
    assert_eq!(
        run_wrapper(
            CLIPPY_WRAPPER,
            None,
            Rustc::Stable,
            &["{root}/rustc", "--crate-name", "dependency"]
        ),
        "RUSTC_BOOTSTRAP=unset\n--crate-name\ndependency\n",
        "dependencies build as the toolchain builds them"
    );
    assert_eq!(
        run_wrapper(
            CLIPPY_WRAPPER,
            None,
            Rustc::Stable,
            &[
                "{root}/clippy-driver",
                "{root}/rustc",
                "-",
                "--crate-name",
                "___",
                "--print=cfg"
            ]
        ),
        "RUSTC_BOOTSTRAP=unset\n{root}/rustc\n-\n--crate-name\n___\n--print=cfg\n",
        "clippy's target queries see a stable compiler"
    );
    let workspace_wrapper = format!("{{root}}/{WRAPPER}");
    assert_eq!(
        run_wrapper(
            CLIPPY_WRAPPER,
            None,
            Rustc::Stable,
            &[&workspace_wrapper, "{root}/rustc", "--crate-name", "demo"]
        ),
        "RUSTC_BOOTSTRAP=demo\n--crate-name\ndemo\n-Zremap-cwd-prefix=.\n-Zallow-features=\n",
        "an ordinary build of the crate gets the flags once"
    );
}
