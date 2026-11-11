use super::*;
use crate::{
    client::connection::ProtocolMismatch,
    protocol::{self, Body, ErrorCode, RemoteError, Request, Response},
};
use tokio::net::UnixListener;

/// What `setup` returns when the daemon answers its first request with
/// `response`.
async fn setup_answered(mut response: Response) -> Result<i32> {
    let temp = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(temp.path());
    paths.prepare()?;
    let listener = UnixListener::bind(&paths.socket)?;
    let daemon = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = protocol::read(&mut stream).await.unwrap();
        response.id = request.id;
        protocol::write(&mut stream, &response).await.unwrap();
    });
    let result = setup(&paths, "worker".into()).await;
    daemon.await?;
    result
}

#[tokio::test]
async fn setup_preserves_inspectable_codes() {
    for code in [
        ErrorCode::ScopeDenied,
        ErrorCode::OperationFailed,
        ErrorCode::ExecutionFailed,
        ErrorCode::ProtocolMismatch,
        ErrorCode::Unknown("future_code".into()),
    ] {
        let response = Response::new(1, Body::error(code.clone(), "failed: detail"));
        let error = setup_answered(response).await.unwrap_err();
        assert!(!error.is::<ProtocolMismatch>());
        let remote = error.downcast_ref::<RemoteError>().unwrap();
        assert_eq!(remote.code, code);
        assert_eq!(remote.message, "failed: detail");
    }
}

#[tokio::test]
async fn setup_reports_a_protocol_mismatch_before_the_error() {
    for code in [
        ErrorCode::ProtocolMismatch,
        ErrorCode::Unknown("future_code".into()),
    ] {
        let mut response = Response::new(1, Body::error(code, "old daemon"));
        response.protocol += 1;
        let error = setup_answered(response).await.unwrap_err();
        assert!(error.is::<ProtocolMismatch>());
        assert!(!error.is::<RemoteError>());
    }
}
