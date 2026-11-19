//! The hub that joins machines' daemons: the links between them, their
//! sessions and streams, the keys that prove who is on each side, and the
//! records of the members a hub admitted and of the hub this machine
//! joined.
pub mod identity;
pub mod links;
pub mod members;
pub mod membership;
pub mod pieces;
pub mod presence;
pub mod sealing;
pub mod session;
pub mod streams;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod transport;

/// The most of what the other side of a link sent that the daemon keeps or
/// shows.
pub const SHOWN_TEXT: usize = 300;

/// `text`, which may quote what the other side of a link sent before it was
/// authenticated, as the daemon keeps and shows it: one line of printable
/// text, at most [`SHOWN_TEXT`] bytes.
pub fn shown(text: &str) -> String {
    let printable: String = (text.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    shoal_protocol::validate::shorten(printable, SHOWN_TEXT)
}

#[cfg(test)]
mod tests;
