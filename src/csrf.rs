//! Cross-site request forgery protection.
//!
//! # The scheme
//!
//! A random token is issued in a cookie and echoed in a hidden field on every
//! form. A `POST` is accepted only when the submitted field matches the token
//! the guard put in the request context, compared in constant time.
//!
//! This is the synchroniser-token pattern with the token delivered in a cookie,
//! which is sound because an attacker who can make a browser send the cookie
//! cannot read it (same-origin, and the session cookie is `HttpOnly`), so they
//! cannot forge the matching field. `SameSite=Lax` on the session and CSRF
//! cookies blocks a cross-site `POST` outright in current browsers; the token
//! is the part that does not depend on browser behaviour.
//!
//! # Why every form is checked in one place
//!
//! The check lives in the extractor, not in each handler, so a new `POST` route
//! cannot be written without it: a handler taking [`CsrfForm`] has already been
//! validated, and one taking [`RawCsrfForm`] has deliberately opted out and
//! says so in its signature.

use std::collections::HashMap;
use std::fmt;

use serde::de::DeserializeOwned;
use topcoat::{
    Result,
    context::{Cx, try_request_context},
    router::{
        Body,
        content::{Form, RawForm},
        error::{BadRequestError, ForbiddenError, bad_request, forbidden},
        request::FromRequest,
    },
};

use crate::auth;
use crate::config::Config;

/// Name of the hidden form field carrying the token.
pub const FIELD: &str = "csrf_token";

/// Name of the cookie carrying the token.
pub const COOKIE: &str = "crm_csrf";

/// How long an issued token stays valid.
///
/// Long enough that a form left open in a tab still submits, short enough that
/// a captured token is not useful indefinitely.
pub const TTL_SECONDS: i64 = 8 * 3600;

/// The token for one request: a random value and when it was minted.
///
/// The encoded form is `value:issued_at`, which is what the cookie carries. The
/// cookie crate percent-encodes the colon on the way out, so no escaping is
/// needed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrfToken {
    /// Random, URL-safe.
    pub value: String,
    /// Unix seconds when the token was issued.
    pub issued_at: i64,
}

impl CsrfToken {
    /// Mint a fresh token.
    #[must_use]
    pub fn fresh() -> Self {
        Self {
            value: auth::random_csrf_token(),
            issued_at: crate::domain::now(),
        }
    }

    /// Whether this token is still inside its validity window.
    ///
    /// A token issued in the future is rejected too, so a clock that jumps
    /// backwards cannot resurrect a stale one.
    #[must_use]
    pub fn is_fresh(&self) -> bool {
        let now = crate::domain::now();
        self.issued_at <= now && now - self.issued_at < TTL_SECONDS
    }

    /// The encoded cookie value.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}:{}", self.value, self.issued_at)
    }

    /// Parse a cookie value, rejecting anything malformed.
    #[must_use]
    pub fn decode(raw: &str) -> Option<Self> {
        let (value, issued_at) = raw.rsplit_once(':')?;
        if value.is_empty() || value.len() > 128 {
            return None;
        }
        Some(Self {
            value: value.to_string(),
            issued_at: issued_at.parse().ok()?,
        })
    }
}

impl fmt::Display for CsrfToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the value itself: it would end up in a log line.
        write!(f, "CsrfToken(issued_at={})", self.issued_at)
    }
}

/// The token this request was issued, if the guard issued one.
///
/// Stored in the request context as the encoded string, which is cheap to clone
/// and is exactly what a form field needs to carry.
#[must_use]
pub fn current(cx: &Cx) -> Option<CsrfToken> {
    encoded_token(cx).and_then(|encoded| CsrfToken::decode(&encoded))
}

/// The request-context key holding the encoded token.
///
/// A newtype rather than a bare `String`, so it cannot collide with some other
/// request-context string.
#[derive(Debug, Clone)]
pub struct TokenContext(pub String);

/// Read the encoded token out of the request context.
#[must_use]
pub fn encoded_token(cx: &Cx) -> Option<String> {
    try_request_context::<TokenContext>(cx).map(|token| token.0.clone())
}

/// The token value to put in a form field, or the empty string.
///
/// A view component must not fail merely because it was built without a
/// request, so the empty string is the graceful answer; submitting it fails
/// validation, which is the safe direction for this to fail in.
#[must_use]
pub fn field_value(cx: &Cx) -> String {
    current(cx).map(|token| token.value).unwrap_or_default()
}

// --- Extractors ------------------------------------------------------------

/// Read a URL-encoded body into the payload, plus the submitted CSRF token.
///
/// The body is read as an untyped map first so the token can be taken out,
/// then the remaining pairs are handed to the same deserializer `Form` uses.
/// Doing it in that order means the payload type does not have to know the
/// token exists, and a payload that is not a struct still works.
fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<(T, String)> {
    let Form(mut fields) = Form::<HashMap<String, String>>::from_bytes(bytes)?;
    let token = fields.remove(FIELD).unwrap_or_default();

    // `serde_urlencoded` escapes the keys and values on the way out, and the
    // empty-value handling the payload cares about is applied on the way in by
    // `Form`, so a blank optional field still reads as `None`.
    let body = serde_urlencoded::to_string(fields)?;
    let Form(payload) = Form::<T>::from_bytes(body.as_bytes())?;
    Ok((payload, token))
}

/// A form submission whose CSRF token has been verified.
///
/// Dereferences to the payload, so a handler reads fields exactly as it would
/// from `topcoat::router::content::Form`.
///
/// Take it as a whole: `body: CsrfForm<NewForm>`. Destructuring it in the
/// parameter position (`CsrfForm(form): CsrfForm<NewForm>`) confuses the route
/// and page macros, which bind the request body to a parameter named `body`.
#[derive(Debug, Clone, Copy, Default)]
#[must_use]
pub struct CsrfForm<T>(pub T);

impl<T> std::ops::Deref for CsrfForm<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for CsrfForm<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<T> for CsrfForm<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> FromRequest for CsrfForm<T>
where
    T: DeserializeOwned,
{
    async fn from_request(cx: &Cx, body: Body) -> Result<Self> {
        let RawForm(bytes) = RawForm::from_request(cx, body).await?;
        let (payload, submitted) = read::<T>(&bytes)?;
        verify(cx, &submitted)?;
        Ok(Self(payload))
    }
}

/// A form whose token is checked but whose payload is optional.
///
/// For a route that must run even when the body is empty or the payload shape
/// is decided at runtime. The token check is not optional.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct RawCsrfForm<T>(pub T);

impl<T> std::ops::Deref for RawCsrfForm<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> FromRequest for RawCsrfForm<T>
where
    T: DeserializeOwned,
{
    async fn from_request(cx: &Cx, body: Body) -> Result<Self> {
        let RawForm(bytes) = RawForm::from_request(cx, body).await?;
        let (payload, submitted) = read::<T>(&bytes)?;
        verify(cx, &submitted)?;
        Ok(Self(payload))
    }
}

/// A `400` for a form body that could not be read at all.
#[must_use]
pub fn malformed_form() -> BadRequestError {
    bad_request("could not read the submitted form")
}

/// Compare a submitted token against the request's, in constant time.
///
/// # Errors
///
/// Returns 403 when the request has no token, or when the submitted one is
/// missing or different. A missing token is not reported differently from a
/// wrong one: the distinction helps an attacker and nobody else.
pub fn verify(cx: &Cx, submitted: &str) -> Result<(), ForbiddenError> {
    let Some(expected) = current(cx) else {
        // Nothing was issued for this request, so nothing can be verified.
        return Err(forbidden());
    };
    if submitted.is_empty() || !auth::constant_time_eq(submitted, &expected.value) {
        return Err(forbidden());
    }
    Ok(())
}

// --- Issuing ---------------------------------------------------------------

/// Issue a token cookie, or reuse the one already present.
///
/// Returns the encoded token so the caller can put it in the request context.
/// This runs in the guard, before the cookie layer, so it reads the incoming
/// header and writes the response header directly.
pub fn issue(cx: &Cx, config: &Config) -> String {
    let existing = auth::cookie_from_headers(cx, COOKIE)
        .and_then(|raw| CsrfToken::decode(&raw))
        .filter(CsrfToken::is_fresh);

    match existing {
        Some(token) => token.encode(),
        None => {
            let token = CsrfToken::fresh();
            let mut cookie = auth::secure_cookie(COOKIE, token.encode(), config);
            cookie.set_max_age(topcoat::cookie::time::Duration::seconds(TTL_SECONDS));
            auth::set_cookie_header(cx, cookie);
            token.encode()
        }
    }
}

/// Whether a `POST` to `path` is a form submission that must carry a token.
///
/// Every `POST` does. Kept as a named predicate so the guard reads as a policy
/// rather than as a hard-coded body check.
#[must_use]
pub fn path_requires_token(method: &topcoat::router::Method, path: &str) -> bool {
    let _ = path;
    !matches!(
        *method,
        topcoat::router::Method::GET | topcoat::router::Method::HEAD | topcoat::router::Method::OPTIONS
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use topcoat::context::CxTestBuilder;

    fn cx_with_token(encoded: Option<&str>) -> Cx {
        let mut builder = CxTestBuilder::new();
        if let Some(encoded) = encoded {
            builder = builder.request_context(TokenContext(encoded.to_string()));
        }
        builder.build()
    }

    #[test]
    fn tokens_round_trip_through_their_cookie_encoding() {
        let token = CsrfToken::fresh();
        let decoded = CsrfToken::decode(&token.encode()).expect("round trips");
        assert_eq!(decoded, token);
    }

    #[test]
    fn tokens_are_unique_and_long_enough() {
        let a = CsrfToken::fresh();
        let b = CsrfToken::fresh();
        assert_ne!(a.value, b.value);
        assert!(a.value.len() >= 32, "{}", a.value);
    }

    #[test]
    fn malformed_cookie_values_are_rejected() {
        assert!(CsrfToken::decode("").is_none());
        assert!(CsrfToken::decode("no-separator").is_none());
        assert!(CsrfToken::decode("value:not-a-number").is_none());
        assert!(CsrfToken::decode(":123").is_none());
        assert!(CsrfToken::decode(&format!("{}:0", "x".repeat(200))).is_none());
    }

    #[test]
    fn freshness_is_bounded_on_both_sides() {
        assert!(CsrfToken::fresh().is_fresh());

        let stale = CsrfToken {
            value: "abc".to_string(),
            issued_at: crate::domain::now() - TTL_SECONDS - 1,
        };
        assert!(!stale.is_fresh());

        // A clock that jumped backwards must not resurrect a stale token.
        let from_the_future = CsrfToken {
            value: "abc".to_string(),
            issued_at: crate::domain::now() + 3600,
        };
        assert!(!from_the_future.is_fresh());
    }

    #[test]
    fn verification_needs_a_matching_token() {
        let token = CsrfToken::fresh();
        let cx = cx_with_token(Some(&token.encode()));

        assert!(verify(&cx, &token.value).is_ok());
        assert!(verify(&cx, "wrong").is_err());
        assert!(verify(&cx, "").is_err());
    }

    #[test]
    fn verification_fails_when_no_token_was_issued() {
        let cx = cx_with_token(None);
        assert!(verify(&cx, "anything").is_err());
    }

    #[test]
    fn field_value_is_empty_without_a_token() {
        assert_eq!(field_value(&cx_with_token(None)), "");
        let token = CsrfToken::fresh();
        assert_eq!(field_value(&cx_with_token(Some(&token.encode()))), token.value);
    }

    #[test]
    fn only_read_only_methods_skip_the_token() {
        use topcoat::router::Method;
        assert!(!path_requires_token(&Method::GET, "/companies"));
        assert!(!path_requires_token(&Method::HEAD, "/companies"));
        assert!(path_requires_token(&Method::POST, "/login"));
        assert!(path_requires_token(&Method::POST, "/companies"));
        assert!(path_requires_token(&Method::POST, "/deals/3/delete"));
    }

    #[test]
    fn debug_output_never_leaks_the_token() {
        let token = CsrfToken::fresh();
        let rendered = format!("{token}");
        assert!(!rendered.contains(&token.value), "{rendered}");
    }

    #[test]
    fn form_payloads_unwrap_from_both_shapes() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Payload {
            name: String,
        }

        // A struct payload carrying the hidden field.
        let (payload, token) = read::<Payload>(b"csrf_token=t&name=Acme").expect("valid");
        assert_eq!(token, "t");
        assert_eq!(
            payload,
            Payload {
                name: "Acme".to_string()
            }
        );

        // A struct payload without it still deserializes, with an empty token
        // that then fails verification rather than being treated as valid.
        let (_, token) = read::<Payload>(b"name=Acme").expect("valid");
        assert_eq!(token, "");

        // A payload that is not a struct deserializes too.
        let (payload, token) = read::<Vec<(String, String)>>(b"a=1&b=2").expect("valid");
        assert_eq!(token, "");
        assert_eq!(payload.len(), 2);
    }
}
