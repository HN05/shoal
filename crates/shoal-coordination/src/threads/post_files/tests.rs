use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_store::test_support::seeds;
use std::path::Path;

use super::*;
use crate::stand_in::Fixture;

#[tokio::test]
async fn a_post_s_files_come_back_in_order_without_those_whose_record_is_gone() -> Result<()> {
    let fixture = &Fixture::new().await;
    fixture
        .store()
        .write(|tx| {
            seeds::ready_workspace(tx, "repo", "work", "worker")?;
            tx.execute(
                "INSERT INTO threads(id,space_id,space_number,number,kind,name,created_at,opener)
                    VALUES (1,'work',1,1,'question','Which?',1,'space')",
                [],
            )?;
            tx.execute(
                "INSERT INTO thread_posts(id,thread_id,created_at,sender,text)
                    VALUES (1,1,1,'space','Here')",
                [],
            )?;
            let space = SpaceId::from("work");
            let file =
                |name: &str| seeds::space_file(tx, &space, name, &Path::new("/f").join(name));
            let (first, gone, last) = (file("first")?, file("gone")?, file("last")?);
            set(tx, 1, &[last, gone, first])?;
            tx.execute("DELETE FROM space_files WHERE id=?1", [gone])?;
            let names: Vec<_> = (of_posts(tx, &[1])?
                .remove(&1)
                .unwrap_or_default()
                .into_iter())
            .map(|file| file.name)
            .collect();
            assert_eq!(names, ["last", "first"]);
            assert_eq!(ids(tx, 1)?, [last, gone, first]);
            set(tx, 1, &[first])?;
            assert_eq!(ids(tx, 1)?, [first]);
            Ok(())
        })
        .await
}
