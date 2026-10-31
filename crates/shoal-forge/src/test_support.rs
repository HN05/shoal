//! Test fixtures: a forge on a loopback port, and accounts that sign in to it.
use std::io::Write as _;

/// A request a [`Server`] received.
#[derive(Debug, Clone)]
pub struct Received {
    /// `METHOD path`, the path with its query.
    pub line: String,
    /// Each header line as sent.
    pub headers: Vec<String>,
    pub body: String,
}

impl Received {
    /// The value of header `name`, matched in any letter case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find_map(|header| {
            let (key, value) = header.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}

/// A reply: status, extra header lines and body.
pub type Reply = (u16, Vec<String>, Vec<u8>);

/// A forge on a loopback port that answers each request with what `reply`
/// returns for it, and keeps every request it received.
pub struct Server {
    /// `127.0.0.1:<port>`.
    pub host: String,
    received: std::sync::Arc<std::sync::Mutex<Vec<Received>>>,
}

impl Server {
    pub fn start(reply: impl Fn(&Received) -> Reply + Send + 'static) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let received = std::sync::Arc::<std::sync::Mutex<Vec<Received>>>::default();
        let log = received.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let (status, headers, body) = reply(&request);
                log.lock().unwrap().push(request);
                let mut head = format!(
                    "HTTP/1.1 {status} Scripted\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for header in headers {
                    head += &format!("{header}\r\n");
                }
                let _ = stream
                    .write_all(format!("{head}\r\n").as_bytes())
                    .and_then(|()| stream.write_all(&body));
            }
        });
        Self { host, received }
    }

    /// Every request received so far.
    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Received> {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let line = format!("{} {}", parts.next()?, parts.next()?);
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end().to_owned();
        if header.is_empty() {
            break;
        }
        headers.push(header);
    }
    let mut request = Received {
        line,
        headers,
        body: String::new(),
    };
    let length = request
        .header("Content-Length")
        .and_then(|length| length.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    request.body = String::from_utf8_lossy(&body).into_owned();
    Some(request)
}

/// The repository `path` of a `kind` forge that `server` stands in for.
pub fn repo_on(server: &Server, kind: crate::backends::ForgeKind, path: &str) -> crate::ForgeRepo {
    crate::ForgeRepo {
        kind,
        host: server.host.clone(),
        path: path.into(),
        web_scheme: "http",
    }
}

/// A token that is always the one it holds.
pub struct Fixed(pub &'static str);

impl crate::account::Token for Fixed {
    fn fetch(&self) -> futures_util::future::BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move { Ok(self.0.to_owned()) })
    }
}

/// An agent account whose token for `server`'s host is `token`.
pub fn agent_on(server: &Server, token: &'static str) -> crate::account::Account {
    let mut logins = crate::account::Logins::default();
    logins.insert(
        &server.host,
        crate::account::Login {
            token: Box::new(Fixed(token)),
            email: None,
        },
    );
    let agent = crate::account::AgentLogins {
        logins,
        separate: Vec::new(),
    };
    crate::account::Account::agent(agent, Default::default())
}
