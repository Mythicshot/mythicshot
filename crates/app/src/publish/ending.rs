//! Ending a token at its own site: when a profile is removed, or its token is replaced by "Sign
//! in again", the site is asked to end the token it no longer needs, so the connection does not
//! stay listed and valid there until it lapses.
//!
//! The call is `DELETE {server}/api/v1/token` with `Authorization: Bearer <the token to end>` and
//! no body. 204: the token is ended. 401: it was not valid any more (ended, lapsed or unknown),
//! which is as good. 404 or 405: an older site without the call. Anything else, or no answer:
//! not done.
//!
//! It is asked once, never again, with a short timeout ([`END_TIMEOUT`]): it must never hold
//! anyone up. Like every other request it goes to the given site only and never follows a
//! redirect ([`Client`]), and nothing of the token is ever part of what it says.

use std::time::Duration;

use super::{Client, Method, Timeouts};

/// The route that ends the token it is called with.
pub const END_PATH: &str = "/api/v1/token";
/// The longest the call may take, connecting included.
pub const END_TIMEOUT: Duration = Duration::from_secs(5);

/// What the site said to ending a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 204: the token is ended.
    Ended,
    /// 401: the token was not valid any more. Nothing is left to end.
    AlreadyEnded,
    /// 404 or 405: the site does not have the call yet.
    SiteTooOld,
    /// Anything else, or no answer: why, in a fixed sentence that never holds the token.
    NotReached(String),
}

/// Ask the site at `server` to end `token`, once, within [`END_TIMEOUT`]. Blocks: never call it
/// on the UI thread.
pub fn end_token(server: &str, token: &str) -> Outcome {
    end_with(server, token, Timeouts { connect: END_TIMEOUT, request: END_TIMEOUT })
}

fn end_with(server: &str, token: &str, timeouts: Timeouts) -> Outcome {
    // A test build only ever talks to the stand-in sites on 127.0.0.1.
    #[cfg(test)]
    if !server.trim().starts_with("http://127.0.0.1:") {
        return Outcome::NotReached("a test build sends nothing to a real site".into());
    }
    let client = match Client::new(server, token, timeouts) {
        Ok(c) => c,
        // The address rule's or the token rule's own sentence: neither holds the token.
        Err(e) => return Outcome::NotReached(e.trim_end_matches('.').to_string()),
    };
    match client.send(Method::Delete, END_PATH, None) {
        Err(t) => Outcome::NotReached(t.0.to_string()),
        Ok(reply) => match reply.status {
            204 => Outcome::Ended,
            401 => Outcome::AlreadyEnded,
            404 | 405 => Outcome::SiteTooOld,
            s @ 300..=399 => Outcome::NotReached(format!("the site answered with a redirect (HTTP {s}), which is not followed")),
            s => Outcome::NotReached(format!("the site answered HTTP {s}")),
        },
    }
}

#[cfg(test)]
mod tests;
