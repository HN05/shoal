use super::*;
use std::path::Path;

fn database() -> Result<Connection> {
    let db = shoal_store::test_support::migrated()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES
            ('repo','/repo','/repo',0), ('other','/other','/other',0);
         INSERT INTO spaces(id,kind,repository_id,name)
             VALUES ('owner','workspace','repo','worker');
         INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
             VALUES ('owner','/worker','topic','ready','abc','/admin');",
    )?;
    Ok(db)
}

#[test]
fn a_branch_belongs_to_one_workspace_of_its_repository() -> Result<()> {
    let mut db = database()?;
    let tx = db.transaction()?;
    let error = ensure_branch_available(&tx, &"repo".into(), Some("topic"), None).unwrap_err();
    assert_eq!(Reason::of(&error), Some(Reason::NameTaken));
    ensure_branch_available(&tx, &"other".into(), Some("topic"), None)?;
    ensure_branch_available(&tx, &"repo".into(), Some("unused"), None)?;
    ensure_branch_available(&tx, &"repo".into(), None, None)?;
    // A rename keeps its own branch.
    let owner = SpaceId::from("owner");
    ensure_branch_available(&tx, &"repo".into(), Some("topic"), Some(&owner))?;
    Ok(())
}

#[test]
fn a_chosen_directory_name_belongs_to_one_workspace_of_any_repository() -> Result<()> {
    let db = database()?;
    // The owner's directory, `/worker`, has the name in another repository's directory too.
    let error = ensure_directory_free(&db, Path::new("/other/worker")).unwrap_err();
    assert!(
        error.to_string().contains("run the command again"),
        "{error:#}"
    );
    ensure_directory_free(&db, Path::new("/other/worker-2"))?;
    Ok(())
}
