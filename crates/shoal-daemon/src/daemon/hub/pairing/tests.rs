use super::{Codes, code_proof, proves_code};

/// Whether `codes` admits a joiner that holds `code`.
fn admits(codes: &Codes, code: &str) -> bool {
    let proof = code_proof(code, b"signed");
    codes.take_proven(|issued| proves_code(issued, b"signed", &proof))
}

#[test]
fn a_code_admits_one_join() {
    let codes = Codes::default();
    let code = codes.issue();
    assert!(admits(&codes, &code));
    assert!(!admits(&codes, &code));
}

#[test]
fn an_unknown_code_admits_none() {
    let codes = Codes::default();
    codes.issue();
    assert!(!admits(&codes, "ABCDEFGHJKLMNPQRSTUV"));
}

#[test]
fn a_code_proof_holds_only_for_what_the_joiner_signed() {
    let proof = code_proof("ABCD", b"signed");
    assert!(proves_code("ABCD", b"signed", &proof));
    assert!(!proves_code("ABCD", b"another transcript", &proof));
}

#[tokio::test(start_paused = true)]
async fn an_expired_code_admits_none() {
    let codes = Codes::default();
    let code = codes.issue();
    tokio::time::advance(super::LIFETIME).await;
    assert!(!admits(&codes, &code));
}
