use super::*;
use crate::client::space_ids::SpaceId;

fn database() -> Result<Connection> {
    let mut db = Connection::open_in_memory()?;
    migrate(&mut db)?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES
            ('repo','/repo','/repo',0), ('other','/other','/other',0);
         INSERT INTO workspaces(id,repository_id,name,path,branch,state) VALUES
            ('owner','repo','worker','/worker','topic','ready');",
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
    let error = ensure_directory_free(&db, std::path::Path::new("/other/worker")).unwrap_err();
    assert!(
        error.to_string().contains("run the command again"),
        "{error:#}"
    );
    ensure_directory_free(&db, std::path::Path::new("/other/worker-2"))?;
    Ok(())
}
