use super::*;

#[test]
fn list_parses_names_fields_and_labels_after_the_marker() {
    let output = "  name=fix-login\tpid=1\tclients=0\tcreated=1\tcwd=file://host/a\t\
            cmd=shoal internal session\tshoal.agent=claude\tshoal.workspace=w1\n\
            \u{2192} name=other\tpid=2\tclients=1\tcmd=sh -c 'x=y'\n\
            no sessions found in /tmp/zmx-501\n";
    let sessions = parse_list(output);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].0, "fix-login");
    assert_eq!(field(&sessions[0].1, "shoal.workspace"), Some("w1"));
    assert_eq!(field(&sessions[0].1, "shoal.agent"), Some("claude"));
    assert_eq!(sessions[1].0, "other");
    assert_eq!(field(&sessions[1].1, "clients"), Some("1"));
    assert_eq!(field(&sessions[1].1, "cmd"), Some("sh -c 'x=y'"));
}

#[test]
fn session_cleanup_is_scoped_to_its_state_directory() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = root.path().join("state with spaces");
    let other = root.path().join("other-state");
    std::fs::create_dir(&state)?;
    std::fs::create_dir(&other)?;
    let label = state_label(&state)?;
    let owned = parse_list(&format!("name=ours\t{label}\n"));
    let foreign = parse_list(&format!("name=theirs\t{}\n", state_label(&other)?));
    assert!(!label.contains(char::is_whitespace));
    for (_, fields) in owned {
        assert!(in_state(&fields, &label, false));
    }
    for (_, fields) in foreign {
        assert!(!in_state(&fields, &label, true));
    }
    assert!(!in_state(&Vec::new(), &label, false));
    assert!(in_state(&Vec::new(), &label, true));
    Ok(())
}

#[test]
fn state_labels_identify_equivalent_paths_and_reject_missing_directories() -> Result<()> {
    let root = tempfile::tempdir()?;
    let state = root.path().join("state with spaces");
    let alias = root.path().join("alias");
    std::fs::create_dir(&state)?;
    std::fs::create_dir(state.join("child"))?;
    std::os::unix::fs::symlink(&state, &alias)?;
    let label = state_label(&state)?;
    assert!(!label.contains(char::is_whitespace));
    for equivalent in [
        state.join(""),
        state.join("."),
        state.join("child/.."),
        alias,
        state.canonicalize()?,
    ] {
        assert_eq!(label, state_label(&equivalent)?);
        let sessions = parse_list(&format!("name=ours\t{}\n", state_label(&equivalent)?));
        for (_, fields) in sessions {
            assert!(in_state(&fields, &label, false));
        }
    }
    assert_ne!(label, state_label(root.path())?);
    assert!(state_label(&root.path().join("missing")).is_err());
    Ok(())
}

#[test]
fn closing_checks_ownership_even_when_a_name_is_reused() {
    let listed = parse_list("name=reused\tshoal.workspace=w1\tshoal.demo=new-launch\n");
    for (_, fields) in listed {
        assert!(owned_by(
            &fields,
            &super::super::Owner::Workspace("w1".into())
        ));
        assert!(!owned_by(
            &fields,
            &super::super::Owner::Workspace("w2".into())
        ));
        assert!(owned_by(
            &fields,
            &super::super::Owner::Demo("new-launch".into())
        ));
        assert!(!owned_by(
            &fields,
            &super::super::Owner::Demo("old-launch".into())
        ));
    }
}
