use std::process::Command;

use super::*;

/// Runs the prepared wrapper in a workspace root, around a stand-in rustc
/// that prints its arguments and `RUSTC_BOOTSTRAP`, and refuses `-Z`
/// options unless `known` and unless `RUSTC_BOOTSTRAP` allows them for the
/// crate. `{root}` in `args` stands for the root's path.
fn run(bootstrap: Option<&str>, known: bool, args: &[&str]) -> String {
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
    [ {known} = true ] || exit 1
    case ",${{RUSTC_BOOTSTRAP-}}," in *,1,*|*,"$crate",*) ;; *) exit 1 ;; esac ;;
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
    let mut command = Command::new("sh");
    command
        .arg(root.path().join(WRAPPER))
        .arg(&rustc)
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
    String::from_utf8(output.stdout).unwrap()
}

const CRATE: &[&str] = &["--crate-name", "demo", "src/lib.rs"];

#[test]
fn the_wrapper_maps_the_working_directory_for_the_crate_it_builds() {
    let mapped = "--crate-name\ndemo\nsrc/lib.rs\n-Zremap-cwd-prefix=.\n";
    assert_eq!(
        run(None, true, CRATE),
        format!("RUSTC_BOOTSTRAP=demo\n{mapped}-Zallow-features=\n"),
        "the crate the wrapper names gets the flag and no unstable features"
    );
    assert_eq!(
        run(Some("other"), true, CRATE),
        format!("RUSTC_BOOTSTRAP=other,demo\n{mapped}-Zallow-features=\n")
    );
    assert_eq!(
        run(Some("1"), true, CRATE),
        format!("RUSTC_BOOTSTRAP=1\n{mapped}"),
        "a crate already granted keeps its features"
    );
    assert_eq!(
        run(Some("other,demo"), true, CRATE),
        format!("RUSTC_BOOTSTRAP=other,demo\n{mapped}")
    );
}

#[test]
fn the_wrapper_passes_the_output_directory_relative_to_the_workspace() {
    let built = |out_dir: &str| {
        run(None, true, &["--crate-name", "demo", "--out-dir", out_dir])
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
        run(Some("-1"), true, CRATE),
        format!("RUSTC_BOOTSTRAP=-1\n{unchanged}"),
        "a refusal of unstable flags wins"
    );
    assert_eq!(
        run(None, false, CRATE),
        format!("RUSTC_BOOTSTRAP=unset\n{unchanged}"),
        "a rustc without the flag builds as usual"
    );
    assert_eq!(run(None, true, &["-vV"]), "RUSTC_BOOTSTRAP=unset\n-vV\n");
    assert_eq!(
        run(None, true, &["-", "--crate-name", "___", "--print=cfg"]),
        "RUSTC_BOOTSTRAP=unset\n-\n--crate-name\n___\n--print=cfg\n",
        "target queries see a stable compiler"
    );
}
