use shoal_forge_types::ci::checks::{CheckLog, JOB_URL_BYTES, TAIL_BYTES};
use shoal_protocol::protocol::MAX_FRAME;

#[test]
fn the_longest_log_tail_fits_one_protocol_frame_even_when_escaped() {
    let log = CheckLog {
        url: "pr".into(),
        name: "rust".into(),
        job_url: Some("\0".repeat(JOB_URL_BYTES)),
        text: "\0".repeat(TAIL_BYTES),
        truncated: true,
        failed_step: Some("\0".repeat(256)),
    };
    assert!(serde_json::to_vec(&log).unwrap().len() < MAX_FRAME);
}
