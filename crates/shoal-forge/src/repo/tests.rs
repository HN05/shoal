use serde_json::json;

use super::*;

#[test]
fn each_forge_reports_its_default_branch_its_own_way() {
    let repository = json!({"default_branch": "main", "mainbranch": {"name": "trunk"}});
    for (kind, branch) in [(ForgeKind::GitHub, "main"), (ForgeKind::Bitbucket, "trunk")] {
        assert_eq!(kind.default_branch(&repository), Some(branch), "{kind}");
    }
}
