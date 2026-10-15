use super::*;

fn database() -> Result<Connection> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES
            ('repo','/repo','/repo',0), ('other','/other','/other',0);
         INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES
            ('owner','repo','worker','/worker','topic','ready'),
            ('renaming','other','old-name','/renaming','old-branch','reconciling');
         INSERT INTO workspace_renames(workspace_id,name,branch) VALUES
            ('renaming','reserved','new-branch');",
    )?;
    Ok(db)
}

#[test]
fn creation_and_rename_share_global_names_and_repository_branches() -> Result<()> {
    let mut db = database()?;
    let tx = db.transaction()?;
    // Creation and rename differ only in whether the caller's own record is excluded.
    for except_id in [None, Some("renaming")] {
        for repository_id in ["repo", "other"] {
            for (name, diagnostic) in [
                ("worker", "workspace name already exists: worker"),
                (
                    "reserved",
                    "workspace name is reserved by an unfinished rename: reserved",
                ),
            ] {
                let error =
                    ensure_name_and_branch_available(&tx, repository_id, name, "unused", except_id)
                        .unwrap_err();
                assert_eq!(error.to_string(), diagnostic);
            }
        }
        let error =
            ensure_name_and_branch_available(&tx, "repo", "free", "topic", except_id).unwrap_err();
        assert_eq!(
            error.to_string(),
            "branch is already owned by workspace worker"
        );
        ensure_name_and_branch_available(&tx, "other", "free", "topic", except_id)?;
        ensure_name_and_branch_available(&tx, "repo", "free", "unused", except_id)?;
    }
    Ok(())
}

#[test]
fn rename_can_retain_its_own_name_and_branch() -> Result<()> {
    let mut db = database()?;
    let tx = db.transaction()?;
    for (name, branch) in [("worker", "topic"), ("worker", "new"), ("new", "topic")] {
        ensure_name_and_branch_available(&tx, "repo", name, branch, Some("owner"))?;
        assert!(ensure_name_and_branch_available(&tx, "repo", name, branch, None).is_err());
    }
    Ok(())
}
