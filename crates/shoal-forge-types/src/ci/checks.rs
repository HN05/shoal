//! The log of a failed CI check, shortened to its tail for reading.

use serde::{Deserialize, Serialize};

/// The most of a log's end a [`CheckLog`] keeps, in bytes.
pub const TAIL_BYTES: usize = 8192;

/// The most of one line a [`CheckLog`]'s tail keeps, in bytes.
const LINE_BYTES: usize = 1024;

/// The longest job URL a [`CheckLog`] keeps, in bytes.
pub const JOB_URL_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLog {
    /// The PR's URL.
    pub url: String,
    pub name: String,
    /// The web page of the check's job, unless longer than [`JOB_URL_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_url: Option<String>,
    pub text: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
}

impl CheckLog {
    /// The [`Self::tail`] of the log `text`.
    pub fn from_text(url: &str, name: &str, text: &str, failed_step: Option<String>) -> Self {
        Self::whole(url, name, text.into(), failed_step).tail()
    }

    /// The whole log `text`.
    pub fn whole(url: &str, name: &str, text: String, failed_step: Option<String>) -> Self {
        Self {
            url: url.into(),
            name: name.into(),
            job_url: None,
            text,
            truncated: false,
            failed_step,
        }
    }

    /// The log's last whole lines within [`TAIL_BYTES`], each longer than
    /// `LINE_BYTES` shortened to its start and end, so that one long line
    /// leaves room for the lines around it; marked truncated when it left
    /// anything out.
    pub fn tail(mut self) -> Self {
        let mut lines = Vec::new();
        let mut size = 0;
        for line in self.text.split_inclusive('\n').rev() {
            let line = shortened(line);
            if size + line.len() > TAIL_BYTES {
                break;
            }
            size += line.len();
            lines.push(line);
        }
        lines.reverse();
        let tail = lines.concat();
        self.truncated |= tail.len() < self.text.len();
        self.text = tail;
        self
    }

    pub fn with_job(self, job_url: String) -> Self {
        Self {
            job_url: Some(job_url).filter(|url| url.len() <= JOB_URL_BYTES),
            ..self
        }
    }
}

/// What stands for the middle a long line's tail leaves out.
const ELLIPSIS: &str = " … ";

/// `line` within [`LINE_BYTES`], its middle left out when it is longer.
fn shortened(line: &str) -> std::borrow::Cow<'_, str> {
    if line.len() <= LINE_BYTES {
        return line.into();
    }
    let kept = (LINE_BYTES - ELLIPSIS.len()) / 2;
    let start = line.floor_char_boundary(kept);
    let end = line.ceil_char_boundary(line.len() - kept);
    format!("{}{ELLIPSIS}{}", &line[..start], &line[end..]).into()
}

#[cfg(test)]
mod tests;
