use anyhow::Result;
use shoal_protocol::paths::Paths;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixListener;

use super::relay;

#[tokio::test]
async fn a_relay_carries_what_the_daemon_sent_before_closing_while_input_still_comes() -> Result<()>
{
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let (mut input, relayed_input) = tokio::io::duplex(64);
    // Room for one byte keeps the reply in the relay until the test reads
    // it, after the relay has failed to write the input to the daemon.
    let (relayed_output, mut output) = tokio::io::duplex(1);
    let relaying = tokio::spawn(async move { relay(&paths, relayed_input, relayed_output).await });
    let (mut daemon, _) = listener.accept().await?;
    daemon.write_all(b"{\"reply\":\"left\"}\n").await?;
    drop(daemon);
    while input.write_all(b"{\"report\":{}}\n").await.is_ok() {}
    let mut carried = String::new();
    output.read_to_string(&mut carried).await?;
    assert_eq!(carried, "{\"reply\":\"left\"}\n");
    relaying.await?
}
