//! One-shot notices across a redirect.
//!
//! A handler that changes something ends in `303 See Other`, so the message
//! explaining what happened cannot ride on that response — the browser throws
//! it away and re-requests the target. The message therefore travels in a
//! short-lived cookie that the layout reads and clears on the *next* render.
//!
//! The cookie is not signed. It is a display string that this app chooses, and
//! the layout escapes it on the way into the HTML, so a forged value can put
//! whatever the forger likes in their own browser's banner and nothing else.

use topcoat::{
    context::Cx,
    cookie::{time::Duration, Cookie, SameSite},
    router::request,
};

use crate::auth;
use crate::config::Config;

/// How the notice should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ok,
    Warn,
    Error,
}

impl Kind {
    /// The value the notice cookie stores.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Ok => "ok",
            Kind::Warn => "warn",
            Kind::Error => "error",
        }
    }

    fn parse(value: &str) -> Kind {
        match value {
            "warn" => Kind::Warn,
            "error" => Kind::Error,
            _ => Kind::Ok,
        }
    }
}

/// A notice waiting to be rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flash {
    pub kind: Kind,
    pub message: String,
}

const COOKIE: &str = "crm_flash";

/// Queue a notice for the next page this browser loads.
pub fn set(cx: &Cx, config: &Config, kind: Kind, message: &str) {
    let value = format!("{}|{}", kind.as_str(), sanitize(message));
    let mut cookie = auth::secure_cookie(COOKIE, value, config);
    // `HttpOnly` would be fine here, but the value is user-visible text either
    // way; the important attribute is the short lifetime.
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(Duration::seconds(60));
    auth::set_cookie_header(cx, cookie);
}

/// Take the pending notice, if there is one, and clear it.
///
/// Clearing happens on read, so a refresh does not show the same notice twice.
/// The guard runs before the cookie layer, so this reads the request header
/// directly and writes the removal onto the response headers.
#[must_use]
pub fn take(cx: &Cx) -> Option<Flash> {
    let raw = auth::cookie_from_headers(cx, COOKIE)?;
    let (kind, message) = raw.split_once('|')?;
    if message.is_empty() {
        return None;
    }

    // Expire it in the browser so a second render does not repeat it.
    let mut removal = Cookie::new(COOKIE.to_string(), String::new());
    removal.set_path("/");
    removal.set_max_age(Duration::seconds(0));
    auth::set_cookie_header(cx, removal);

    Some(Flash {
        kind: Kind::parse(kind),
        message: sanitize(message),
    })
}

/// Keep a message to one line and a sane length.
///
/// Newlines are stripped rather than escaped because a banner is a single line
/// by design; a message that needed a paragraph is a message that should have
/// been a page.
fn sanitize(message: &str) -> String {
    let flattened: String = message
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let collapsed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(300).collect()
}

/// Whether a request carries a pending notice, without consuming it.
///
/// Only used to decide whether to render the banner container.
#[must_use]
pub fn peek(cx: &Cx) -> Option<Flash> {
    let raw = auth::cookie_from_headers(cx, COOKIE)?;
    let (kind, message) = raw.split_once('|')?;
    (!message.is_empty()).then(|| Flash {
        kind: Kind::parse(kind),
        message: sanitize(message),
    })
}

/// Whether this request is a form submission that a notice may follow.
#[must_use]
pub fn follows_a_redirect(cx: &Cx) -> bool {
    !matches!(
        *request::method(cx),
        topcoat::router::Method::GET | topcoat::router::Method::HEAD
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_flattened_to_one_line() {
        assert_eq!(sanitize("a\nb"), "a b");
        assert_eq!(sanitize("  spaced   out  "), "spaced out");
        assert_eq!(sanitize("trailing\r\n"), "trailing");
    }

    #[test]
    fn messages_are_bounded() {
        let long = "x".repeat(1000);
        assert_eq!(sanitize(&long).chars().count(), 300);
    }

    #[test]
    fn kinds_round_trip_and_default_to_ok() {
        for kind in [Kind::Ok, Kind::Warn, Kind::Error] {
            assert_eq!(Kind::parse(kind.as_str()), kind);
        }
        assert_eq!(Kind::parse("nonsense"), Kind::Ok);
    }
}
