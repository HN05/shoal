//! Migration 324: each post's list of files moved into its table.
use super::*;

#[test]
fn each_post_lists_its_files_in_their_table_in_order() -> Result<()> {
    let mut db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('work','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name,number)
            VALUES ('work','workspace','repo','worker',3);
        COMMIT;
        INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener)
            VALUES (1,'work',3,1,'question','Which?',1,'space');
        INSERT INTO thread_posts(id,thread_id,created_at,sender,text,files)
            VALUES (1,1,1,'space','None','[]'), (2,1,2,'space','Two','[7,5]'),
                (3,1,3,'space','Gone','[9]');",
    )?;
    migrate(&mut db)?;
    let mut reading =
        db.prepare("SELECT post_id,position,file_id FROM thread_post_files ORDER BY rowid")?;
    let listed = reading
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // A file whose record is gone keeps its place, as the list kept it.
    assert_eq!(listed, [(2, 0, 7), (2, 1, 5), (3, 0, 9)]);
    Ok(())
}

#[test]
fn a_post_whose_files_are_no_list_fails_the_migration() -> Result<()> {
    let mut db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('work','/work','worker','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name) VALUES ('work','workspace','repo','worker');
        COMMIT;
        INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener)
            VALUES (1,'work',3,1,'question','Which?',1,'space');
        INSERT INTO thread_posts(id,thread_id,created_at,sender,text,files)
            VALUES (4,1,1,'space','Bad','{\"id\":1}');",
    )?;
    let error = format!("{:#}", migrate(&mut db).unwrap_err());
    assert!(error.contains("thread_posts row 4"), "{error}");
    Ok(())
}
