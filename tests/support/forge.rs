//! The scripted forge from `fixtures/forge.py`: a `curl` that answers GitHub's
//! API from `forge/responses.json`, and an HTTP server that serves Forgejo's.
use serde_json::Value;
use std::{fs, path::Path, process::Child};

use crate::support;

/// Install the scripted forge in `root` as the `curl` on every Shoal process's
/// PATH, answering no request until a test scripts it.
pub fn install(root: &Path) {
    fs::create_dir_all(root.join("forge")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    respond(root, &Value::Object(Default::default()));
    support::install_test_script(root.join("forge.py"), include_str!("../fixtures/forge.py"));
    support::install_test_script(root.join("bin/curl"), include_str!("../fixtures/curl"));
    fs::write(
        root.join("forge/tls.pem"),
        include_str!("../fixtures/forge-tls.pem"),
    )
    .unwrap();
}

/// Answer later forge requests in `root` with `responses`, replacing the
/// earlier ones.
pub fn respond(root: &Path, responses: &Value) {
    fs::write(root.join("forge/responses.json"), responses.to_string()).unwrap();
}

/// The scripted Forgejo server, stopped when dropped.
pub struct Server {
    child: Child,
    /// The `host:port` it listens on.
    pub host: String,
}

impl Server {
    /// Start the server installed in `root`.
    pub fn start(root: &Path) -> Self {
        Self::serve(root, "port", &[])
    }

    /// Start the server installed in `root` over HTTPS, with a certificate
    /// for `127.0.0.1`, the one [`Server::certificate`] names.
    pub fn start_tls(root: &Path) -> Self {
        Self::serve(root, "tls-port", &[Self::certificate(root)])
    }

    /// The certificate the HTTPS server presents, to trust.
    pub fn certificate(root: &Path) -> String {
        root.join("forge/tls.pem").to_str().unwrap().to_owned()
    }

    /// Start the server, writing its port to `forge/<port>`, with `args`
    /// after the port file.
    fn serve(root: &Path, port: &str, args: &[String]) -> Self {
        let port_file = root.join("forge").join(port);
        // Owned before it listens, so a failed start also stops it.
        let mut server = Self {
            child: support::isolated(root, "python3")
                .arg(root.join("forge.py"))
                .args(["serve", port_file.to_str().unwrap()])
                .args(args)
                .spawn()
                .unwrap(),
            host: String::new(),
        };
        // A hang guard only: the server writes its port once it listens.
        support::wait::wait_until("forge server to start", || port_file.exists());
        let port = fs::read_to_string(port_file).unwrap();
        server.host = format!("127.0.0.1:{port}");
        server
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// GitHub's search request for `query`, page `page`.
pub fn github_search(query: &str, page: u32) -> String {
    let query: String = query
        .bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect();
    format!("GET search/issues?q={query}&sort=created&order=desc&per_page=100&page={page}")
}
