use super::Codes;

#[test]
fn a_code_admits_one_join() {
    let codes = Codes::default();
    let code = codes.issue();
    assert!(codes.take(&code));
    assert!(!codes.take(&code));
}

#[test]
fn an_unknown_code_admits_none() {
    let codes = Codes::default();
    codes.issue();
    assert!(!codes.take("ABCDEFGHJKLMNPQRSTUV"));
}

#[tokio::test(start_paused = true)]
async fn an_expired_code_admits_none() {
    let codes = Codes::default();
    let code = codes.issue();
    tokio::time::advance(super::LIFETIME).await;
    assert!(!codes.take(&code));
}
