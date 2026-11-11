use std::{collections::BTreeMap, fs};

use super::*;
use crate::config::presets::environment;

#[test]
fn preset_variables_give_way_to_configured_ones() {
    let configured = BTreeMap::from([("GOFLAGS".to_owned(), "-mod=vendor".to_owned())]);
    assert_eq!(environment(&[Preset::Go], configured.clone()), configured);
    assert_eq!(
        environment(&[Preset::Cargo, Preset::Go], BTreeMap::new())["GOFLAGS"],
        "-trimpath"
    );
}

#[test]
fn the_xcode_preset_keeps_its_files_in_a_shared_cache() {
    // A configured cache of the same tool name is the user's own.
    let root = tempfile::tempdir().unwrap();
    prepare_cache("xcode", root.path()).unwrap();
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    let env = environment(&[Preset::Xcode], BTreeMap::new());
    assert_eq!(
        env["XCODE_XCCONFIG_FILE"],
        format!("{{cache.{XCODE_FILES_CACHE}}}/{}", xcode::XCCONFIG)
    );
    assert_eq!(
        env[crate::env::PATH_PREFIX],
        format!("{{cache.{XCODE_FILES_CACHE}}}/{}", xcode::BIN)
    );
}

#[test]
fn the_cargo_preset_runs_workspace_crates_through_its_wrappers() {
    let root = tempfile::tempdir().unwrap();
    prepare_cache(CARGO_CACHE, root.path()).unwrap();
    let env = environment(&[Preset::Cargo], BTreeMap::new());
    for (variable, wrapper) in [
        ("RUSTC_WORKSPACE_WRAPPER", cargo::WRAPPER),
        ("RUSTC_WRAPPER", cargo::CLIPPY_WRAPPER),
    ] {
        assert!(root.path().join(wrapper).is_file());
        assert_eq!(env[variable], format!("{{cache.{CARGO_CACHE}}}/{wrapper}"));
    }
    // A wrapper of the user's own in `[env]` replaces the clippy wrapper.
    let own = BTreeMap::from([("RUSTC_WRAPPER".to_owned(), "sccache".to_owned())]);
    assert_eq!(
        environment(&[Preset::Cargo], own)["RUSTC_WRAPPER"],
        "sccache"
    );
}
