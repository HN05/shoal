use super::*;

#[test]
fn statuses_report_each_ref_as_git_push_did() {
    let refspecs = vec![
        "HEAD:refs/heads/topic".to_owned(),
        "+refs/heads/b:refs/heads/b".to_owned(),
        ":refs/heads/gone".to_owned(),
    ];
    let porcelain = "To git@forge:o/r.git\n\
        *\tHEAD:refs/heads/topic\t[new branch]\n\
        !\trefs/heads/b:refs/heads/b\t[rejected] (stale info)\n\
        Done\n";
    assert_eq!(
        statuses(&refspecs, porcelain),
        "ok refs/heads/topic\n\
         error refs/heads/b [rejected] (stale info)\n\
         error refs/heads/gone git push failed\n"
    );
}

#[test]
fn options_accept_what_a_push_passes_on_and_refuse_a_dry_run() {
    let mut session = Session {
        remote: "origin".into(),
        url: "git@forge:o/r.git".into(),
        listed: BTreeMap::new(),
        leases: BTreeMap::new(),
        pushes: Vec::new(),
    };
    assert_eq!(session.option("progress false"), "ok\n");
    assert_eq!(session.option("cas refs/heads/topic:abc"), "ok\n");
    assert_eq!(session.option("cas refs/heads/new:"), "ok\n");
    assert_eq!(session.option("dry-run true"), "unsupported\n");
    assert_eq!(session.leases["refs/heads/topic"], "abc");
    assert_eq!(session.leases["refs/heads/new"], "");
}
