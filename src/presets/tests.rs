use super::*;
use std::fs;

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
fn zig_and_ccache_presets_point_their_caches_past_workspace_paths() {
    let presets = [Preset::Zig, Preset::Ccache];
    assert_eq!(
        caches(&presets, BTreeMap::new())["preset.zig"].scope,
        CacheScope::Shared
    );
    let env = environment(&presets, BTreeMap::new());
    assert_eq!(env["ZIG_LOCAL_CACHE_DIR"], "{cache.preset.zig}");
    assert_eq!(env["CCACHE_BASEDIR"], "{workspace_path}");
    assert_eq!(env["CMAKE_CXX_COMPILER_LAUNCHER"], "ccache");
}

#[test]
fn the_xcode_preset_keeps_its_files_in_a_shared_cache() {
    let caches = caches(&[Preset::Xcode], BTreeMap::new());
    assert_eq!(caches[XCODE_CACHE].scope, CacheScope::Shared);
    // A configured cache of the same tool name is the user's own.
    let root = tempfile::tempdir().unwrap();
    prepare_cache("xcode", root.path()).unwrap();
    assert!(fs::read_dir(root.path()).unwrap().next().is_none());
    let env = environment(&[Preset::Xcode], BTreeMap::new());
    assert_eq!(
        env["XCODE_XCCONFIG_FILE"],
        format!("{{cache.{XCODE_CACHE}}}/{}", xcode::XCCONFIG)
    );
    assert_eq!(
        env[crate::env::PATH_PREFIX],
        format!("{{cache.{XCODE_CACHE}}}/{}", xcode::BIN)
    );
}
