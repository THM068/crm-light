//! Authentication: password hashing, sessions, two-factor support, and the
//! guard layer every request passes through.
//!
//! # Session model
//!
//! A login mints a 32-byte random token, stores the user in the request context
//! and puts the token in a `SameSite=Lax`, `HttpOnly` cookie. The database only
//! ever sees the token's SHA-256 digest, so a leaked database snapshot is not a
//! set of usable sessions, and logging out revokes server-side instead of
//! relying on the browser forgetting.
//!
//! # Layer order
//!
//! [`Guard`] is registered without a path, which makes it the outermost layer:
//! it runs *before* `topcoat-cookie`'s root layer, so it cannot use the cookie
//! jar. It reads the incoming `Cookie` header itself and puts outgoing cookies
//! on the router's deferred response headers. Handlers further in, which do run
//! behind the cookie layer, can use the ordinary cookie helpers freely.
//!
//! # Guard
//!
//! The guard resolves the session, rejects a request that has none, and hands
//! the rest of the chain a context carrying [`CurrentUser`]. [`is_public`] is
//! the single allow-list, so a new page is protected by default.

use std::sync::Arc;

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use toasty::Db;
use topcoat::{
    Result,
    context::{Cx, try_request_context},
    cookie::{Cookie, SameSite, time::Duration as CookieDuration},
    router::{
        Body, HeaderName, HeaderValue, Layer, LayerFuture, Next, Path,
        error::{internal_server_error, redirect, forbidden, unauthorized},
        request,
        response::{IntoResponse, response_headers},
    },
};

use crate::config::Config;
use crate::csrf;
use crate::domain::{self, Role};
use crate::models::{Account, LoginAttempt, Session, User};

/// Name of the cookie carrying the session token.
pub const SESSION_COOKIE: &str = "crm_session";

/// Minimum accepted password length, in characters.
pub const MIN_PASSWORD_LEN: usize = 9;

/// Longest username accepted.
pub const MAX_USERNAME_LEN: usize = 64;

/// Digits in a TOTP code.
const TOTP_DIGITS: u32 = 6;

/// TOTP time step, in seconds.
const TOTP_PERIOD: i64 = 30;

/// How many steps either side of "now" a submitted code may be.
///
/// One step of slack absorbs clock drift and the time it takes to type a code,
/// without widening the guessing window meaningfully.
const TOTP_SKEW_STEPS: i64 = 1;

// --- Current user ----------------------------------------------------------

/// The signed-in user, as carried in the request context.
///
/// Cheap to clone and free of secrets, so it rides along on every request and
/// can be read from any view component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    pub id: i64,
    /// The workspace this person belongs to. Every query they make is scoped to
    /// it; see [`crate::access`].
    pub account_id: i64,
    /// Username as stored, for display.
    pub username: String,
    pub display_name: Option<String>,
    pub role: Role,
    /// The workspace's display name, for the chrome.
    pub account_name: String,
    /// The workspace's sign-in slug, which is what `/login` matches on.
    pub account_slug: String,
}

impl CurrentUser {
    pub fn from_model(user: &User, account: &Account) -> Self {
        Self {
            id: user.id,
            account_id: user.account_id,
            username: user.username.clone(),
            display_name: user.display_name.clone(),
            role: Role::from_stored(&user.role),
            account_name: account.name.clone(),
            account_slug: account.slug.clone(),
        }
    }

    /// The tenant, as [`crate::access`] wants it.
    pub fn tenant(&self) -> crate::access::Tenant {
        crate::access::Tenant {
            account_id: self.account_id,
            user_id: self.id,
        }
    }

    /// Name to show in the chrome: the display name when set, else the
    /// username.
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }

    /// First letter of the display name, for the avatar chip.
    pub fn initial(&self) -> String {
        self.name()
            .chars()
            .next()
            .map(|c| c.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "?".to_string())
    }

    pub fn is_admin(&self) -> bool {
        self.role.is_admin()
    }
}

/// The signed-in user, or `None` for an anonymous request.
///
/// Only meaningful inside a handler: the guard installs the value. It reports
/// *who* the request is for; it is not a substitute for [`require_admin`].
#[must_use]
pub fn current_user(cx: &Cx) -> Option<CurrentUser> {
    try_request_context::<CurrentUser>(cx).cloned()
}

/// The signed-in user, or a 401.
///
/// # Errors
///
/// Returns an unauthorized response when the request carries no identity.
pub fn require_user(cx: &Cx) -> Result<CurrentUser> {
    current_user(cx).ok_or_else(|| unauthorized().into())
}

/// The signed-in user, or a 403 unless they are an administrator **of their own
/// workspace**.
///
/// The role is a per-tenant thing: an administrator manages the accounts in
/// their workspace and has no standing in any other.
///
/// # Errors
///
/// Returns 401 when the request is anonymous, and 403 when the user is signed
/// in but is not an administrator.
pub fn require_admin(cx: &Cx) -> Result<CurrentUser> {
    let user = require_user(cx)?;
    if user.is_admin() {
        Ok(user)
    } else {
        Err(forbidden().into())
    }
}

// --- Tokens ----------------------------------------------------------------

/// A fresh 256-bit token, base64 encoded.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    BASE64.encode(bytes)
}

/// A fresh random CSRF token value.
#[must_use]
pub fn random_csrf_token() -> String {
    random_token()
}

/// Hex SHA-256 of a session token.
///
/// The digest is what the database stores; the lookup is an exact match on an
/// indexed column.
#[must_use]
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Compare two strings without leaking their common prefix length.
#[must_use]
pub fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    left.ct_eq(right).into()
}

// --- Passwords -------------------------------------------------------------

/// Hash a password with Argon2id and a fresh random salt.
///
/// # Errors
///
/// Returns a message when hashing fails, which in practice means the process is
/// out of memory.
pub fn hash_password(password: &str) -> std::result::Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| format!("could not hash password: {error}"))
}

/// Verify a password against a stored PHC string.
///
/// A malformed stored hash returns `false` rather than an error: a corrupt row
/// must not authenticate anyone, and every failure looks identical from the
/// outside.
#[must_use]
pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Burn the same work as a real verification, for a username that does not
/// exist.
///
/// Without this, "no such user" returns in microseconds while a real account
/// takes tens of milliseconds, which is a username oracle.
pub fn dummy_password_check(password: &str) {
    // Argon2id PHC string for the literal "crm-light-dummy-value". The value is
    // irrelevant; only the cost of the attempt matters.
    const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$Y3JtLWxpZ2h0LWR1bW15$ZG8tbm90LW1hdGNoLWFueXRoaW5n";
    let _ = verify_password(password, DUMMY);
}

/// Whether a stored hash represents "no password set".
#[must_use]
pub fn has_password(stored_hash: Option<&str>) -> bool {
    stored_hash.is_some_and(|hash| !hash.trim().is_empty())
}

/// The password policy, in one place both the form and the handler can ask.
///
/// Returns a human-readable reason when the password is too short or too
/// obvious. There is deliberately no composition rule: length beats character
/// classes, and the message says so.
#[must_use]
pub fn password_problem(password: &str) -> Option<String> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Some(format!(
            "Use at least {MIN_PASSWORD_LEN} characters; a few words in a row is fine."
        ));
    }
    let lowered = password.to_ascii_lowercase();
    const BLOCKLIST: [&str; 8] = [
        "password",
        "123456",
        "qwerty",
        "letmein",
        "admin123",
        "crm-light",
        "changeme",
        "iloveyou",
    ];
    if BLOCKLIST.iter().any(|bad| lowered.contains(bad)) {
        return Some("That password contains a very common word or sequence.".to_string());
    }
    if password.chars().all(|c| c.is_ascii_digit()) {
        return Some("Use something other than digits alone.".to_string());
    }
    None
}

// --- Two-factor (TOTP, RFC 6238) -------------------------------------------

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Generate a new base32 TOTP secret (160 bits, the RFC 4226 recommendation).
#[must_use]
pub fn generate_totp_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    base32_encode(&bytes)
}

/// Encode bytes as RFC 4648 base32 without padding, the form authenticator
/// apps expect.
fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    for chunk in bytes.chunks(5) {
        let mut buffer = [0u8; 5];
        buffer[..chunk.len()].copy_from_slice(chunk);

        let bits = (u64::from(buffer[0]) << 32)
            | (u64::from(buffer[1]) << 24)
            | (u64::from(buffer[2]) << 16)
            | (u64::from(buffer[3]) << 8)
            | u64::from(buffer[4]);

        // Five input bytes become eight base32 characters; a short final chunk
        // produces proportionally fewer.
        let characters = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for index in 0..characters {
            let shift = 35 - index * 5;
            out.push(BASE32_ALPHABET[((bits >> shift) & 0x1f) as usize] as char);
        }
    }
    out
}

/// Decode RFC 4648 base32, tolerating lowercase and missing padding.
fn base32_decode(input: &str) -> Option<Vec<u8>> {
    let mut bits: u64 = 0;
    let mut bit_count: u32 = 0;
    let mut out = Vec::with_capacity(input.len() * 5 / 8);

    for ch in input.chars() {
        if ch == '=' || ch.is_whitespace() {
            continue;
        }
        let value = match ch.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u8 - b'A',
            c @ '2'..='7' => c as u8 - b'2' + 26,
            _ => return None,
        };
        bits = (bits << 5) | u64::from(value);
        bit_count += 5;
        if bit_count >= 8 {
            bit_count -= 8;
            out.push((bits >> bit_count) as u8);
        }
    }

    Some(out)
}

/// The TOTP code for `secret` at `counter`.
fn totp_code(secret: &[u8], counter: u64) -> Option<u32> {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret).ok()?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    // Dynamic truncation, RFC 4226 §5.3.
    let offset = (digest[19] & 0x0f) as usize;
    let binary = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);

    Some(binary % 10u32.pow(TOTP_DIGITS))
}

/// Verify a submitted TOTP code against a stored base32 secret.
///
/// Allows [`TOTP_SKEW_STEPS`] steps either side of the current one, so a code
/// entered just as the step rolls over still works.
#[must_use]
pub fn verify_totp(secret_base32: &str, code: &str, unix_seconds: i64) -> bool {
    let Some(secret) = base32_decode(secret_base32) else {
        return false;
    };
    if secret.is_empty() {
        return false;
    }
    let Some(submitted) = normalize_totp_code(code) else {
        return false;
    };

    let counter = unix_seconds.div_euclid(TOTP_PERIOD);
    let mut matched = false;
    for step in -TOTP_SKEW_STEPS..=TOTP_SKEW_STEPS {
        let Some(candidate) = counter.checked_add(step) else {
            continue;
        };
        if candidate < 0 {
            continue;
        }
        let Some(expected) = totp_code(&secret, candidate as u64) else {
            continue;
        };
        // Every candidate is compared, so the running time does not reveal
        // which step matched.
        let expected = format!("{expected:0width$}", width = TOTP_DIGITS as usize);
        matched |= constant_time_eq(&expected, &submitted);
    }
    matched
}

/// Accept `123456` and `123 456`, rejecting anything else.
fn normalize_totp_code(code: &str) -> Option<String> {
    let digits: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (digits.len() == TOTP_DIGITS as usize && digits.chars().all(|c| c.is_ascii_digit()))
        .then_some(digits)
}

/// The `otpauth://` URI an authenticator app scans.
#[must_use]
pub fn totp_provisioning_uri(issuer: &str, account: &str, secret_base32: &str) -> String {
    let label = urlencode(&format!("{issuer}:{account}"));
    let issuer = urlencode(issuer);
    format!(
        "otpauth://totp/{label}?secret={secret_base32}&issuer={issuer}&digits={TOTP_DIGITS}&period={TOTP_PERIOD}"
    )
}

/// Percent-encode everything outside the unreserved set, as RFC 3986 requires
/// for a query value.
#[must_use]
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// --- Sessions --------------------------------------------------------------

/// Mint a session for `user_id` and return the raw token.
///
/// The token is returned, not stored: the caller puts it in the cookie while
/// the database keeps only its digest.
pub async fn create_session(
    db: &mut Db,
    config: &Config,
    user_id: i64,
    account_id: i64,
    user_agent: Option<&str>,
    ip: Option<&str>,
) -> toasty::Result<String> {
    let token = random_token();
    let now = domain::now();

    toasty::create!(Session {
        token_hash: token_hash(&token),
        account_id,
        user_id,
        created_at: now,
        expires_at: now + config.session_ttl.as_secs() as i64,
        revoked_at: None,
        user_agent: user_agent.map(|value| truncate(value, 300)),
        ip: ip.map(|value| truncate(value, 64)),
    })
    .exec(db)
    .await?;

    Ok(token)
}

/// Resolve a session token to its user and workspace, if the session is live.
///
/// Live means: the row exists, is not revoked, has not expired, its user is
/// still active, and the session's `account_id` agrees with the user's. That
/// last check is redundant with how sessions are created, and it is kept
/// because it is the one place where a mismatch would silently widen a
/// tenant's reach.
pub async fn resolve_session(
    db: &mut Db,
    token: &str,
) -> toasty::Result<Option<(Session, User, Account)>> {
    let digest = token_hash(token);
    let Some(session) = Session::filter(Session::fields().token_hash().eq(digest))
        .first()
        .exec(db)
        .await?
    else {
        return Ok(None);
    };

    if session.revoked_at.is_some() || session.expires_at <= domain::now() {
        return Ok(None);
    }

    let Some(user) = User::filter(User::fields().id().eq(session.user_id))
        .first()
        .exec(db)
        .await?
    else {
        return Ok(None);
    };

    if !user.active {
        return Ok(None);
    }

    // The session's tenant must agree with its user's, or the request would be
    // scoped to a workspace its user is not in.
    if session.account_id != user.account_id {
        return Ok(None);
    }

    let Some(account) = Account::filter(Account::fields().id().eq(user.account_id))
        .first()
        .exec(db)
        .await?
    else {
        return Ok(None);
    };

    Ok(Some((session, user, account)))
}

/// Revoke one session by token.
pub async fn revoke_session(db: &mut Db, token: &str) -> toasty::Result<()> {
    let digest = token_hash(token);
    if let Some(mut session) = Session::filter(Session::fields().token_hash().eq(digest))
        .first()
        .exec(db)
        .await?
    {
        toasty::update!(session { revoked_at: Some(domain::now()) })
            .exec(db)
            .await?;
    }
    Ok(())
}

/// Revoke every live session for a user, optionally sparing one token.
///
/// Used after a password change (sparing the current session, so changing your
/// own password does not sign you out) and when an administrator resets someone
/// else's credentials.
pub async fn revoke_user_sessions(
    db: &mut Db,
    user_id: i64,
    except_token: Option<&str>,
) -> toasty::Result<usize> {
    let except_digest = except_token.map(token_hash);
    let now = domain::now();
    let mut revoked = 0;

    let sessions = Session::filter(
        Session::fields()
            .user_id()
            .eq(user_id)
            .and(Session::fields().revoked_at().is_none()),
    )
    .exec(db)
    .await?;

    for mut session in sessions {
        if except_digest.as_deref() == Some(session.token_hash.as_str()) {
            continue;
        }
        toasty::update!(session { revoked_at: Some(now) })
            .exec(db)
            .await?;
        revoked += 1;
    }

    Ok(revoked)
}

/// Delete sessions that expired or were revoked more than `grace_seconds` ago.
///
/// Keeps the table from growing without bound; revoked rows are kept briefly so
/// an audit view can still show them.
pub async fn purge_sessions(db: &mut Db, grace_seconds: i64) -> toasty::Result<usize> {
    let cutoff = domain::now() - grace_seconds;
    let mut removed = 0;

    let dead = Session::filter(
        Session::fields()
            .expires_at()
            .lt(cutoff)
            .or(Session::fields().revoked_at().lt(Some(cutoff))),
    )
    .exec(db)
    .await?;

    for session in dead {
        Session::delete_by_id(db, session.id).await?;
        removed += 1;
    }

    Ok(removed)
}

/// Truncate a string to `max` characters, for free-text columns.
fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

// --- Failed-login throttle --------------------------------------------------

/// How long sign-in for this username stays refused, if it is locked.
///
/// Scoped to the workspace, so one tenant cannot lock another's users out.
pub async fn login_lockout(
    db: &mut Db,
    account_id: i64,
    username_lower: &str,
) -> toasty::Result<Option<i64>> {
    let Some(attempt) = find_attempt(db, account_id, username_lower).await? else {
        return Ok(None);
    };
    match attempt.locked_until {
        Some(until) if until > domain::now() => Ok(Some(until - domain::now())),
        _ => Ok(None),
    }
}

/// Record a failed sign-in, locking the account once the threshold is crossed.
pub async fn record_login_failure(
    db: &mut Db,
    config: &Config,
    account_id: i64,
    username_lower: &str,
) -> toasty::Result<()> {
    let now = domain::now();
    match find_attempt(db, account_id, username_lower).await? {
        Some(mut attempt) => {
            let failures = attempt.failures + 1;
            let locked_until =
                (failures >= config.login_max_attempts).then(|| now + config.login_lockout.as_secs() as i64);
            toasty::update!(attempt {
                failures,
                locked_until,
                last_failure_at: now,
            })
            .exec(db)
            .await?;
        }
        None => {
            let locked_until =
                (config.login_max_attempts <= 1).then(|| now + config.login_lockout.as_secs() as i64);
            toasty::create!(LoginAttempt {
                account_id,
                username_lower,
                failures: 1,
                locked_until,
                last_failure_at: now,
            })
            .exec(db)
            .await?;
        }
    }
    Ok(())
}

/// Clear the failure counter after a successful sign-in.
pub async fn clear_login_failures(
    db: &mut Db,
    account_id: i64,
    username_lower: &str,
) -> toasty::Result<()> {
    if let Some(attempt) = find_attempt(db, account_id, username_lower).await? {
        LoginAttempt::delete_by_id(db, attempt.id).await?;
    }
    Ok(())
}

async fn find_attempt(
    db: &mut Db,
    account_id: i64,
    username_lower: &str,
) -> toasty::Result<Option<LoginAttempt>> {
    LoginAttempt::filter(
        LoginAttempt::fields()
            .account_id()
            .eq(account_id)
            .and(
                LoginAttempt::fields()
                    .username_lower()
                    .eq(username_lower.to_string()),
            ),
    )
    .first()
    .exec(db)
    .await
}

// --- Cookies ---------------------------------------------------------------

/// Build a cookie with the attributes every auth cookie here shares.
#[must_use]
pub fn secure_cookie(name: &str, value: String, config: &Config) -> Cookie<'static> {
    let mut cookie = Cookie::new(name.to_string(), value);
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(config.cookie_secure);
    cookie
}

/// The session cookie for a freshly minted token.
///
/// The cookie carries the workspace slug as a second field so the sign-in form
/// can be prefilled on the next visit: usernames are only unique inside a
/// workspace, so the slug is half of the credential and asking for it twice
/// would be a needless obstacle.
#[must_use]
pub fn session_cookie(token: String, config: &Config, account_slug: &str) -> Cookie<'static> {
    let mut cookie = secure_cookie(SESSION_COOKIE, format!("{token}|{account_slug}"), config);
    cookie.set_max_age(CookieDuration::seconds(config.session_ttl.as_secs() as i64));
    cookie
}

/// An expiring cookie that clears the session in the browser.
#[must_use]
pub fn clear_session_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE.to_string(), String::new());
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(CookieDuration::seconds(0));
    cookie
}

/// Read the session token straight from the request headers.
///
/// The guard runs outside the cookie layer, so it cannot use the cookie jar and
/// parses the incoming `Cookie` headers itself. The token and the workspace slug
/// travel in one cookie, separated by a `|`, which cannot appear in either.
#[must_use]
pub fn session_token(cx: &Cx) -> Option<String> {
    cookie_from_headers(cx, SESSION_COOKIE).map(|value| {
        value
            .split('|')
            .next()
            .unwrap_or_default()
            .to_string()
    })
}

/// The workspace slug the last sign-in used, for prefilling the login form.
#[must_use]
pub fn last_account_slug(cx: &Cx) -> Option<String> {
    let raw = cookie_from_headers(cx, SESSION_COOKIE)?;
    let (_, slug) = raw.split_once('|')?;
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Read one cookie value from the request's `Cookie` headers.
#[must_use]
pub fn cookie_from_headers(cx: &Cx, name: &str) -> Option<String> {
    for header in request::headers(cx).get_all(topcoat::router::header::COOKIE) {
        let Ok(raw) = header.to_str() else {
            continue;
        };
        for pair in raw.split(';') {
            let Some((key, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if key.trim() == name {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Queue a `Set-Cookie` header on the response being built.
///
/// Errors are ignored deliberately: a cookie value that `HeaderValue` refuses
/// could never be sent, and failing the whole request over it would be worse
/// than losing one cookie.
pub fn set_cookie_header(cx: &Cx, cookie: Cookie<'_>) {
    if let Ok(value) = HeaderValue::from_str(&cookie.to_string()) {
        response_headers(cx).append(HeaderName::from_static("set-cookie"), value);
    }
}

// --- Guard layer -----------------------------------------------------------

/// Normalise a request path for comparison.
///
/// `/login/` and `/login` reach the same handler, so a trailing slash is
/// trimmed — but trimming `/` yields the empty string, which must not be
/// confused with any real route.
fn normalize_path(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}

/// The pages a signed-in visitor is redirected away from.
///
/// Sign-in and sign-up: somebody who already has a session has no business on
/// either, and reaching `/signup` while signed in would otherwise create a
/// second workspace by accident. Assets such as the stylesheet are reachable
/// without a session too, but they are not pages to be navigated to, so they
/// must keep working once somebody has signed in.
#[must_use]
pub fn redirects_when_signed_in(path: &str) -> bool {
    matches!(normalize_path(path), "/login" | "/signup")
}

/// Whether an anonymous request may be served this path.
///
/// Everything not listed here requires a session, so forgetting to protect a
/// new page is a visible omission in one list rather than a silent hole.
///
/// Sign-in page, plus the static assets the layout references. A stylesheet
/// that only loads for anonymous requests is worse than useless: it fails
/// exactly where it is needed.
#[must_use]
pub fn reachable_without_session(path: &str) -> bool {
    matches!(
        normalize_path(path),
        "/login" | "/signup" | "/style.css" | "/favicon.ico"
    )
}

/// Rejects anonymous requests and installs [`CurrentUser`] for the rest.
pub struct Guard {
    db: Db,
    config: Arc<Config>,
}

impl Guard {
    /// Build the layer around a database handle and resolved configuration.
    #[must_use]
    pub fn new(db: Db, config: Arc<Config>) -> Self {
        Self { db, config }
    }

    /// The query value that tells the login page where to go afterwards.
    fn login_location(cx: &Cx) -> String {
        let target = request::uri(cx)
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/");
        match safe_return_target(target) {
            Some(target) if target != "/" => {
                format!("/login?next={}", urlencode(&target))
            }
            _ => "/login".to_string(),
        }
    }
}

impl Layer for Guard {
    fn path(&self) -> Option<&Path> {
        None
    }

    fn handle<'a>(&'a self, cx: &'a Cx, body: Body, next: Next<'a>) -> LayerFuture<'a> {
        Box::pin(async move {
            let path = request::uri(cx).path().to_string();

            // A CSRF token is issued for every request, before anything else,
            // so the login form has one too. Reusing an unexpired cookie keeps
            // the value stable across requests, which is what lets a form
            // rendered on one page be submitted to another.
            let csrf = csrf::issue(cx, &self.config);
            let cx = cx.with(csrf::TokenContext(csrf));

            let token = session_token(&cx);
            let mut db = self.db.clone();
            let user = match token.as_deref().filter(|token| !token.is_empty()) {
                Some(token) => match resolve_session(&mut db, token).await {
                    Ok(Some((_, user, account))) => Some((user, account)),
                    Ok(None) => {
                        // Stale, revoked, expired, or a deactivated account:
                        // drop the cookie so the browser stops replaying it.
                        set_cookie_header(&cx, clear_session_cookie());
                        None
                    }
                    Err(error) => return Err(internal_server_error(error).into()),
                },
                None => None,
            };

            let Some((user, account)) = user else {
                if reachable_without_session(&path) {
                    return next.run(&cx, body).await;
                }
                return redirect(Self::login_location(&cx)).into_response(&cx);
            };

            if redirects_when_signed_in(&path) {
                // Already signed in: send them on rather than showing the form.
                // Every other anonymous-reachable path — the stylesheet above
                // all — is served normally, or the page would render unstyled
                // the moment somebody signed in.
                let target = request::uri(&cx)
                    .query()
                    .and_then(return_target)
                    .unwrap_or_else(|| "/".to_string());
                return redirect(target).into_response(&cx);
            }

            // Defence in depth on every response, not just the HTML pages: the
            // stylesheet is served from the same origin and browsers sniff.
            response_headers(&cx).append(
                HeaderName::from_static("x-content-type-options"),
                HeaderValue::from_static("nosniff"),
            );
            response_headers(&cx).append(
                HeaderName::from_static("x-frame-options"),
                HeaderValue::from_static("DENY"),
            );
            response_headers(&cx).append(
                HeaderName::from_static("referrer-policy"),
                HeaderValue::from_static("same-origin"),
            );

            let current = CurrentUser::from_model(&user, &account);
            next.run(&cx.with(current), body).await
        })
    }
}

/// Keep a `?next=…` value only when it is a same-origin absolute path.
///
/// This is what stops `?next=https://evil.example` from turning the login page
/// into an open redirect.
#[must_use]
pub fn safe_return_target(candidate: &str) -> Option<String> {
    let trimmed = candidate.trim();
    // Must be root-relative: no scheme, no protocol-relative `//host`, no
    // backslashes, no header-injection characters.
    if !trimmed.starts_with('/')
        || trimmed.starts_with("//")
        || trimmed.contains('\\')
        || trimmed.contains(['\n', '\r'])
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Pull a `next=…` value out of a raw query string.
fn return_target(query: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "next").then(|| percent_decode(value)).flatten()
    })
}

/// Decode `%XX` escapes and `+` as space.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok().and_then(|value| safe_return_target(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hashes_round_trip() {
        let hash = hash_password("correct horse battery staple").expect("hashing works");
        assert!(hash.starts_with("$argon2id$"), "{hash}");
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn password_hashes_are_salted() {
        let first = hash_password("same password twice").expect("hashing works");
        let second = hash_password("same password twice").expect("hashing works");
        assert_ne!(first, second, "each hash must use a fresh salt");
        assert!(verify_password("same password twice", &first));
        assert!(verify_password("same password twice", &second));
    }

    #[test]
    fn malformed_stored_hash_never_authenticates() {
        assert!(!verify_password("anything", "not-a-phc-string"));
        assert!(!verify_password("anything", ""));
    }

    #[test]
    fn password_policy_rejects_weak_input() {
        assert!(password_problem("short").is_some());
        assert!(password_problem("password123").is_some());
        assert!(password_problem("1234567890").is_some());
        assert!(password_problem("a long enough passphrase").is_none());
    }

    #[test]
    fn missing_password_is_distinguishable_from_a_blank_one() {
        assert!(!has_password(None));
        assert!(!has_password(Some("")));
        assert!(!has_password(Some("   ")));
        assert!(has_password(Some("$argon2id$…")));
    }

    #[test]
    fn usernames_are_normalised_for_lookup() {
        assert_eq!(domain::normalize_username("  Ada  "), "ada");
        assert!(domain::username_is_well_formed("ada.lovelace"));
        assert!(!domain::username_is_well_formed(""));
        assert!(!domain::username_is_well_formed("has space"));
        assert!(!domain::username_is_well_formed(&"x".repeat(65)));
    }

    #[test]
    fn roles_default_to_the_least_privilege() {
        assert_eq!(Role::from_stored("admin"), Role::Admin);
        assert_eq!(Role::from_stored("member"), Role::Member);
        assert_eq!(Role::from_stored("superuser"), Role::Member);
    }

    #[test]
    fn base32_round_trips() {
        for bytes in [vec![], vec![0x41], vec![0x41, 0x42], vec![1, 2, 3, 4, 5]] {
            let encoded = base32_encode(&bytes);
            assert_eq!(base32_decode(&encoded).as_deref(), Some(&bytes[..]));
        }
        // Known vector from RFC 4648: "foobar" → "MZXW6YTBOI".
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn totp_matches_rfc_6238_vectors() {
        // The RFC's SHA-1 test secret is the ASCII string "12345678901234567890".
        let secret = base32_encode(b"12345678901234567890");
        for (time, expected) in [
            (59_i64, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert!(
                verify_totp(&secret, expected, time),
                "code {expected} should verify at {time}"
            );
        }
    }

    #[test]
    fn totp_tolerates_one_step_of_drift() {
        let secret = base32_encode(b"12345678901234567890");
        // The code for t=59 is 287082; at t=89 it is one step stale but still
        // inside the skew window.
        assert!(verify_totp(&secret, "287082", 89));
        // Two steps later it is not.
        assert!(!verify_totp(&secret, "287082", 149));
    }

    #[test]
    fn totp_rejects_nonsense() {
        let secret = base32_encode(b"12345678901234567890");
        assert!(!verify_totp(&secret, "abcdef", 59));
        assert!(!verify_totp(&secret, "12345", 59));
        assert!(!verify_totp("not base32!!", "287082", 59));
        assert!(!verify_totp("", "287082", 59));
        // Whitespace inside a pasted code is tolerated.
        assert!(verify_totp(&secret, "287 082", 59));
    }

    #[test]
    fn session_tokens_are_unique_and_hashed() {
        let a = random_token();
        let b = random_token();
        assert_ne!(a, b);
        assert_eq!(token_hash(&a), token_hash(&a));
        assert_ne!(token_hash(&a), token_hash(&b));
        assert_eq!(token_hash(&a).len(), 64);
    }

    #[test]
    fn constant_time_eq_behaves_like_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn return_targets_must_stay_on_this_origin() {
        assert_eq!(safe_return_target("/companies"), Some("/companies".into()));
        assert_eq!(safe_return_target("/deals?q=x"), Some("/deals?q=x".into()));
        assert_eq!(safe_return_target("https://evil.example"), None);
        assert_eq!(safe_return_target("//evil.example"), None);
        assert_eq!(safe_return_target("/\\evil"), None);
        assert_eq!(safe_return_target("companies"), None);
        assert_eq!(safe_return_target("/a\nb"), None);
    }

    #[test]
    fn percent_decoding_keeps_only_safe_targets() {
        assert_eq!(
            percent_decode("%2Fdeals%3Fq%3Da%20b").as_deref(),
            Some("/deals?q=a b")
        );
        // A decoded value that turns out to be off-origin is dropped.
        assert_eq!(percent_decode("https%3A%2F%2Fevil.example"), None);
        assert_eq!(percent_decode("%zz"), None);
    }

    #[test]
    fn only_the_authentication_pages_are_reachable_without_a_session() {
        assert!(reachable_without_session("/login"));
        assert!(reachable_without_session("/login/"));
        assert!(reachable_without_session("/signup"));
        assert!(reachable_without_session("/style.css"));
        assert!(reachable_without_session("/favicon.ico"));
        assert!(!reachable_without_session("/"));
        assert!(!reachable_without_session("/companies"));
        assert!(!reachable_without_session("/admin/users"));
        // The empty path is what trimming `/` would produce; it must not be
        // mistaken for any real route, least of all the login page.
        assert!(!reachable_without_session(""));
    }

    #[test]
    fn assets_stay_anonymous_but_are_not_redirected_away() {
        // The distinction the guard turns on: `/style.css` is reachable without
        // a session *and* must keep being served with one. Treating the two
        // lists as one made the stylesheet 307 to `/` after sign-in, so every
        // page rendered unstyled.
        for asset in ["/style.css", "/favicon.ico"] {
            assert!(reachable_without_session(asset), "{asset}");
            assert!(!redirects_when_signed_in(asset), "{asset}");
        }
    }

    #[test]
    fn the_authentication_pages_bounce_a_signed_in_visitor() {
        assert!(redirects_when_signed_in("/login"));
        assert!(redirects_when_signed_in("/login/"));
        // Signing up while already signed in would quietly create a second
        // workspace, so it bounces too.
        assert!(redirects_when_signed_in("/signup"));
        assert!(!redirects_when_signed_in("/"));
        assert!(!redirects_when_signed_in("/logins"));
        assert!(!redirects_when_signed_in("/admin/login"));
    }

    #[test]
    fn provisioning_uri_is_scannable() {
        let uri = totp_provisioning_uri("crm-light", "admin", "ABCDEFGH");
        assert!(uri.starts_with("otpauth://totp/crm-light%3Aadmin?"), "{uri}");
        assert!(uri.contains("secret=ABCDEFGH"), "{uri}");
        assert!(uri.contains("issuer=crm-light"), "{uri}");
    }
}
