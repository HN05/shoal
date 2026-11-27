//! Pairing codes: what `shoal hub pair` prints, and `shoal hub join` or an
//! app reads.
use std::{fmt, str::FromStr};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_states::text_key;

use super::{Fingerprint, HubUrl};
use crate::time::UnixTime;

/// The path of a pairing code's URL on the hub.
const PATH: &str = "/pair";

/// The path of the installer the hub serves, which installs its version on
/// a computer without Shoal and joins it with a pairing code.
pub const INSTALL_PATH: &str = "/install";

/// The hub's URL, its key's fingerprint and the single-use code, written as
/// `<url>/pair#code=<code>&key=<fingerprint>`. The fragment never reaches a
/// server, so neither the code nor the fingerprint passes through a proxy's
/// logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    pub url: HubUrl,
    pub key: Fingerprint,
    pub code: String,
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { url, key, code } = self;
        write!(f, "{url}{PATH}#code={code}&key={key}")
    }
}

impl FromStr for PairingCode {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let invalid = || {
            format!(
                "{text} is not a pairing code: the code is the whole URL shoal hub pair printed, in quotes, as in shoal hub join '<hub url>{PATH}#code=…&key=…'"
            )
        };
        let (page, fragment) = text.split_once('#').with_context(invalid)?;
        let url = page.strip_suffix(PATH).with_context(invalid)?;
        let (mut code, mut key) = (None, None);
        for field in fragment.split('&') {
            match field.split_once('=') {
                Some(("code", value)) => code = Some(value.to_owned()),
                Some(("key", value)) => key = Some(value.parse()?),
                _ => {}
            }
        }
        let code = code.filter(|code| !code.is_empty()).with_context(invalid)?;
        ensure!(
            code.chars().all(|c| c.is_ascii_alphanumeric()),
            "{}",
            invalid()
        );
        Ok(Self {
            url: url.parse()?,
            key: key.with_context(invalid)?,
            code,
        })
    }
}

impl PairingCode {
    /// The command that joins a machine with Shoal with this code.
    pub fn join_command(&self) -> String {
        format!("shoal hub join {}", quoted(&self.to_string()))
    }

    /// The command that installs the hub's version on a computer without
    /// Shoal and joins it with this code.
    pub fn installer(&self) -> String {
        let script = quoted(&format!("{}{INSTALL_PATH}", self.url));
        format!(
            "curl -fsSL {script} | sh -s -- {}",
            quoted(&self.to_string())
        )
    }
}

/// `text` as one word of a POSIX shell command.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

text_key!(PairingCode);

/// A pairing code the hub issued, which it takes once until `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    pub code: PairingCode,
    pub expires_at: UnixTime,
}
