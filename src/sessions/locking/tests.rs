use super::*;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

#[tokio::test]
async fn socket_aliases_share_an_exclusive_lock_across_states() -> Result<()> {
    let root = TempDir::new()?;
    let server = root.path().join("server.sock");
    std::fs::write(&server, "")?;
    let alias = root.path().join("alias.sock");
    symlink(&server, &alias)?;
    let path = lock_path(&server)?;
    assert_eq!(lock_path(&alias)?, path);
    std::fs::write(&path, "")?;
    let _guard = for_server(&server).await?;
    let contender = open(&lock_path(&alias)?)?;
    assert_eq!(
        fs2::FileExt::try_lock_exclusive(&contender)
            .expect_err("another state cannot interleave ownership changes")
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    Ok(())
}

#[tokio::test]
async fn independent_servers_do_not_block_each_other_even_without_a_socket() -> Result<()> {
    let root = TempDir::new()?;
    let first = root.path().join("first.sock");
    let second = root.path().join("second.sock");
    std::fs::write(lock_path(&first)?, "")?;
    std::fs::write(lock_path(&second)?, "")?;
    let _guard = for_server(&first).await?;
    let contender = open(&lock_path(&second)?)?;
    fs2::FileExt::try_lock_exclusive(&contender)?;
    Ok(())
}
