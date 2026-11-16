use super::https_host;

#[test]
fn answers_only_requests_over_https_that_name_a_host() {
    let https = "protocol=https\nhost=git.example.com:3000\npath=team/repo.git\n";
    let http = "protocol=http\nhost=git.example.com\n";
    let without_host = "protocol=https\n";

    assert_eq!(https_host(https), Some("git.example.com:3000"));
    assert_eq!(https_host(http), None);
    assert_eq!(https_host(without_host), None);
}
