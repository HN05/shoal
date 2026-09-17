//! Documentation guard. Every Markdown file has a line budget here so the docs
//! stay short enough to keep accurate. Trim before raising a budget; a new
//! Markdown file needs an entry and a reason it cannot live in an existing one.
use std::{path::Path, process::Command};

const BUDGETS: &[(&str, usize)] = &[
    ("AGENTS.md", 110),
    ("CLAUDE.md", 5),
    ("README.md", 160),
    ("SKILL.md", 120),
    ("design.md", 240),
    ("docs/reference.md", 580),
    ("docs/releases.md", 40),
];

#[test]
fn markdown_files_are_listed_and_within_their_line_budgets() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "*.md",
        ])
        .output()
        .expect("git ls-files");
    assert!(output.status.success(), "{output:?}");
    let listed = String::from_utf8(output.stdout).unwrap();
    let mut failures = Vec::new();
    for file in listed.split('\0').filter(|f| !f.is_empty()) {
        let Some((_, budget)) = BUDGETS.iter().find(|(name, _)| *name == file) else {
            failures.push(format!(
                "{file}: new Markdown file; fold it into an existing document or budget it in tests/docs.rs"
            ));
            continue;
        };
        let lines = std::fs::read_to_string(root.join(file))
            .unwrap()
            .lines()
            .count();
        if lines > *budget {
            failures.push(format!(
                "{file}: {lines} lines exceed its budget of {budget}; trim before extending"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
