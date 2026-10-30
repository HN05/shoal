//! Unit-test fixtures.
use std::{fs, io::Write as _, path::Path, process::Command};

/// Install `script` at `path` as a new executable file, renamed over any
/// earlier one. A child process writes it: a test that forks while this
/// binary holds the file open for writing leaves the descriptor in its child
/// until the child execs, and running the script meanwhile fails with "Text
/// file busy".
pub fn install_test_script(path: &Path, script: &str) {
    let staging = tempfile::tempdir_in(path.parent().unwrap()).unwrap();
    let staged = staging.path().join("script");
    let mut writer = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(&staged)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = writer.stdin.take().unwrap();
    input.write_all(script.as_bytes()).unwrap();
    drop(input);
    assert!(
        writer.wait().unwrap().success(),
        "install {}",
        path.display()
    );
    fs::rename(&staged, path).unwrap();
}
