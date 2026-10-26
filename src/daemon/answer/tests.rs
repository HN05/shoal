use super::*;
use crate::protocol::MAX_FRAME;

#[tokio::test]
async fn an_oversized_reply_is_answered_with_an_error_saying_so() {
    let (mut writer, reader) = tokio::io::duplex(4 * MAX_FRAME);
    let env = [("VALUE".to_owned(), "x".repeat(MAX_FRAME))].into();
    let error = reply(&mut writer, 7, Body::WorkspaceEnv(env))
        .await
        .unwrap_err();
    assert!(error.is::<protocol::FrameTooLarge>(), "{error:#}");
    drop(writer);
    let response: Response = protocol::read(&mut tokio::io::BufReader::new(reader))
        .await
        .unwrap();
    assert_eq!(response.id, 7);
    let Body::Error { code, message } = response.body else {
        panic!("expected an error reply");
    };
    assert_eq!(code, ErrorCode::OperationFailed);
    assert!(
        message.contains("exceeds the 65536-byte protocol frame"),
        "{message}"
    );
}
