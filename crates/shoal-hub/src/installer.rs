//! The installer the hub's listener serves, which `shoal hub pair` names: a
//! shell script that installs exactly the hub's version from its release on
//! a computer without Shoal, checking its checksum, as a downloaded binary,
//! or `Shoal.app` on macOS, then starts its daemon and joins the hub with the
//! pairing code it is given.
use http_body_util::{BodyExt as _, Full};
use hyper::{
    Response,
    body::Bytes,
    header::{self, HeaderValue},
};

use super::app_servers::Body;

const SCRIPT: &str = include_str!("installer/install.sh");

/// The installer of this hub's version, from the releases under `releases`.
fn script(releases: &str) -> String {
    SCRIPT
        .replace("@VERSION@", env!("CARGO_PKG_VERSION"))
        .replace("@RELEASES@", releases)
}

/// The response that serves the installer.
pub(super) fn response() -> Response<Body> {
    let script = script(shoal_core::update::RELEASES);
    let body = Full::new(Bytes::from(script));
    let mut response = Response::new(body.map_err(|never| match never {}).boxed());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/x-shellscript; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests;
