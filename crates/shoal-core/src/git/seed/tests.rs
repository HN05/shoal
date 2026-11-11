use super::*;
use crate::test_support::git;

#[tokio::test]
async fn outputs_holding_a_tracked_file_are_left_out() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    git(repo, &["init", "-b", "main"]);
    for file in ["vendor/lib.rs", "target/built", "vend/x"] {
        std::fs::create_dir_all(repo.join(file).parent().unwrap()).unwrap();
        std::fs::write(repo.join(file), "x").unwrap();
    }
    git(repo, &["add", "vendor/lib.rs"]);
    let outputs = ["target", "vendor", "vend", "missing", "vendor/", "./vendor"];
    let outputs = outputs.map(String::from);

    let untracked = untracked_outputs(repo, &outputs).await.unwrap();

    assert_eq!(untracked, ["target", "vend", "missing"]);
}

#[test]
fn an_output_holds_files_below_it_however_it_is_spelled() {
    for output in [
        "web/node_modules",
        "web/node_modules/",
        "web//node_modules",
        "./web/node_modules",
    ] {
        assert!(holds(output, "web/node_modules/lib.js"), "{output}");
        assert!(holds(output, "web/node_modules"), "{output}");
        assert!(!holds(output, "web/node_modules_old/lib.js"), "{output}");
        assert!(!holds(output, "web"), "{output}");
    }
    assert!(!holds(".", "anything"));
}
