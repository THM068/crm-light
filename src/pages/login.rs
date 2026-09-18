//! Sign-in, sign-out, and the signed-in user's own account page.
//!
//! `/login` is the only route the guard lets through without a session. Its
//! CSRF token is still checked, against the token cookie rather than against a
//! session, so the form cannot be driven cross-site either. Everything else
//! here — the throttle, the audit row, the session row — is the same as any
//! other handler.

use serde::Deserialize;
use toasty::Db;
use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{RouterErrorExt, SeeOther, bad_request, internal_server_error, see_other},
        header, page, query_params, request, route,
    },
    view::{View, view},
};

use crate::auth::{self};
use crate::config::Config;
use crate::domain;
use crate::flash;
use crate::access;
use crate::models::{Account, Session, User};
use crate::views::{csrf_field, flash_banner};

#[query_params(error = bad_request)]
struct LoginQuery {
    next: Option<String>,
}

/// Where to send a freshly signed-in user: their original target, or the
/// dashboard.
fn landing(next: Option<&str>) -> String {
    next.and_then(auth::safe_return_target)
        .unwrap_or_else(|| "/".to_string())
}

#[page("/login")]
async fn login_form(cx: &Cx) -> Result<impl View> {
    let next = query_params::<LoginQuery>(cx)?
        .next
        .clone()
        .and_then(|value| auth::safe_return_target(&value));
    // Prefilled from the last sign-in on this browser, so most people never
    // have to think about the workspace field.
    let workspace = auth::last_account_slug(cx).unwrap_or_default();

    Ok(view! {
        <div class="auth-card">
            <h1>"Sign in"</h1>
            <p class="muted">"crm-light"</p>

            flash_banner(
                kind: "warn",
                message: "Sign in to reach the CRM. Every page and every action needs an account.",
            )

            <form method="post" action="/login">
                csrf_field()
                <input type="hidden" name="next" value=(next.clone().unwrap_or_default())>
                <div class="field">
                    <label for="workspace">"Workspace"</label>
                    <input id="workspace" name="workspace" value=(workspace) autofocus=""
                           autocapitalize="none" autocorrect="off" spellcheck="false" required=""
                           placeholder="acme-robotics">
                    <span class="hint">
                        "The sign-in name your workspace was created with. Usernames are unique \
                         per workspace, so this is how we know which one you mean."
                    </span>
                </div>
                <div class="field">
                    <label for="username">"Username"</label>
                    <input id="username" name="username" autocomplete="username" required="">
                </div>
                <div class="field">
                    <label for="password">"Password"</label>
                    <input id="password" name="password" type="password" autocomplete="current-password">
                </div>
                <div class="field">
                    <label for="totp">"Two-factor code"</label>
                    <input id="totp" name="totp" inputmode="numeric" autocomplete="one-time-code"
                           placeholder="Only if enabled">
                </div>
                <button class="btn btn-primary" type="submit">"Sign in"</button>
            </form>

            if crate::config_of(cx).allow_signup {
                <p class="muted">"No workspace yet? " <a href="/signup">"Create one"</a></p>
            } else {
                <p class="muted">
                    "This installation is not accepting new workspaces. Ask an administrator
                    of your workspace to add you."
                </p>
            }
        </div>
    })
}

#[derive(Deserialize)]
struct LoginForm {
    #[serde(default)]
    workspace: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    totp: String,
    #[serde(default)]
    next: String,
}

/// Why a sign-in attempt was refused.
///
/// Only the variants with a distinct, safe message reach the user; the rest
/// collapse into one response, so a wrong password, an unknown username, and a
/// closed account cannot be told apart from the outside.
enum Attempt {
    Ok(Box<User>, Box<Account>),
    BadCredentials,
    Locked(i64),
    PasswordlessDisabled,
    TotpRequired,
    TotpWrong,
    Db(toasty::Error),
}

/// The whole sign-in decision, with no HTTP in it.
///
/// # Tenant resolution comes first
///
/// The workspace is looked up before the username, because a username only
/// means anything inside one. A wrong workspace and a wrong username give the
/// same answer — the throttle is per workspace, and the failure is counted
/// against the username the caller claimed — so this cannot be used to discover
/// which workspaces exist beyond the fact that a slug was accepted.
async fn attempt_login(db: &mut Db, config: &Config, form: &LoginForm) -> Attempt {
    let username = form.username.trim();
    if username.is_empty() {
        return Attempt::BadCredentials;
    }
    let key = domain::normalize_username(username);

    let slug = access::slugify(&form.workspace);
    if slug.is_empty() {
        return Attempt::BadCredentials;
    }
    // Named `workspace` rather than `account`: `#[page("/account")]` below
    // generates a unit struct called `account` in this module, so a local
    // binding of that name would resolve to the struct instead of the row.
    let workspace = match Account::filter(Account::fields().slug().eq(slug))
        .first()
        .exec(db)
        .await
    {
        Ok(workspace) => workspace,
        Err(error) => return Attempt::Db(error),
    };
    let Some(workspace) = workspace else {
        // Spend the same time as a real verification, so neither the response
        // time nor the message says whether the workspace exists.
        auth::dummy_password_check(&form.password);
        return Attempt::BadCredentials;
    };
    let account_id = workspace.id;

    match auth::login_lockout(db, account_id, &key).await {
        Ok(Some(remaining)) => return Attempt::Locked(remaining),
        Ok(None) => {}
        Err(error) => return Attempt::Db(error),
    }

    let user = match User::filter(
        User::fields()
            .account_id()
            .eq(account_id)
            .and(User::fields().username_lower().eq(key.clone())),
    )
    .first()
    .exec(db)
    .await
    {
        Ok(user) => user,
        Err(error) => return Attempt::Db(error),
    };

    let Some(user) = user else {
        // Spend the same time as a real verification, so the response time does
        // not reveal whether the username exists.
        auth::dummy_password_check(&form.password);
        if let Err(error) = auth::record_login_failure(db, config, account_id, &key).await {
            return Attempt::Db(error);
        }
        return Attempt::BadCredentials;
    };

    // A closed account is refused exactly like a wrong password: confirming
    // that an account exists is itself worth something to an attacker.
    if !user.active {
        auth::dummy_password_check(&form.password);
        if let Err(error) = auth::record_login_failure(db, config, account_id, &key).await {
            return Attempt::Db(error);
        }
        return Attempt::BadCredentials;
    }

    let password_ok = match user.password_hash.as_deref() {
        Some(hash) if !hash.trim().is_empty() => auth::verify_password(&form.password, hash),
        _ => {
            // No password stored. Accepted only while
            // `CRM_ALLOW_EMPTY_PASSWORD` is on, and only for a blank field, so
            // the bootstrap admin can be signed into on a fresh install.
            if !config.allow_passwordless_login {
                return Attempt::PasswordlessDisabled;
            }
            form.password.is_empty()
        }
    };

    if !password_ok {
        if let Err(error) = auth::record_login_failure(db, config, account_id, &key).await {
            return Attempt::Db(error);
        }
        return Attempt::BadCredentials;
    }

    if let Some(secret) = user.totp_secret.as_deref().filter(|s| !s.is_empty()) {
        if form.totp.trim().is_empty() {
            return Attempt::TotpRequired;
        }
        if !auth::verify_totp(secret, &form.totp, domain::now()) {
            if let Err(error) = auth::record_login_failure(db, config, account_id, &key).await {
                return Attempt::Db(error);
            }
            return Attempt::TotpWrong;
        }
    }

    if let Err(error) = auth::clear_login_failures(db, account_id, &key).await {
        return Attempt::Db(error);
    }
    Attempt::Ok(Box::new(user), Box::new(workspace))
}

#[route(POST "/login")]
async fn login(cx: &Cx, body: crate::csrf::CsrfForm<LoginForm>) -> Result<SeeOther> {
    let config = crate::config_of(cx);
    let crate::csrf::CsrfForm(form) = body;
    let mut db = crate::db(cx);

    let (mut user, workspace) = match attempt_login(&mut db, config, &form).await {
        Attempt::Ok(user, workspace) => (*user, *workspace),
        Attempt::Locked(remaining) => {
            return Err(bad_request(format!(
                "Too many failed sign-in attempts for that account. Try again in {}.",
                humanize_seconds(remaining)
            ))
            .into());
        }
        Attempt::PasswordlessDisabled => {
            return Err(bad_request(
                "That account has no password set, and password-less sign-in is disabled. \
                 Ask an administrator to set one.",
            )
            .into());
        }
        Attempt::TotpRequired => return Err(bad_request("Enter your two-factor code.").into()),
        Attempt::TotpWrong => {
            return Err(bad_request("That two-factor code is not valid.").into());
        }
        Attempt::BadCredentials => {
            return Err(bad_request("Incorrect username or password.").into());
        }
        Attempt::Db(error) => return Err(internal_server_error(error).into()),
    };

    let user_agent = request::headers(cx)
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    // Behind a proxy the left-most entry is the client; without one this is
    // absent and the session simply records no address.
    let ip = request::headers(cx)
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string());

    let token = auth::create_session(
        &mut db,
        config,
        user.id,
        workspace.id,
        user_agent.as_deref(),
        ip.as_deref(),
    )
    .await
    .map_err(internal_server_error)?;

    // Record the sign-in on the account, so the admin list shows activity.
    toasty::update!(user { last_login_at: Some(domain::now()) })
        .exec(&mut db)
        .await?;

    auth::set_cookie_header(cx, auth::session_cookie(token, config, &workspace.slug));

    Ok(see_other(landing(Some(&form.next))))
}

#[route(POST "/logout")]
async fn logout(cx: &Cx) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    if let Some(token) = auth::session_token(cx) {
        // Revoked server-side, not merely forgotten by the browser.
        let _ = auth::revoke_session(&mut db, &token).await;
    }
    auth::set_cookie_header(cx, auth::clear_session_cookie());
    Ok(see_other("/login"))
}

// --- The signed-in user's own account --------------------------------------

#[page("/account")]
async fn account(cx: &Cx) -> Result<impl View> {
    let mut db = crate::db(cx);
    let current = auth::require_user(cx)?;
    let user = User::filter(User::fields().id().eq(current.id))
        .first()
        .exec(&mut db)
        .await?
        .ok_or_not_found()?;

    let sessions = Session::filter(
        Session::fields()
            .user_id()
            .eq(user.id)
            .and(Session::fields().revoked_at().is_none())
            .and(Session::fields().expires_at().gt(domain::now())),
    )
    .exec(&mut db)
    .await?;

    let zone = crate::views::zone(cx);
    let two_factor_on = user
        .totp_secret
        .as_deref()
        .is_some_and(|secret| !secret.is_empty());
    // Regenerated on every render, deliberately: nothing is stored until a code
    // proves the app and the authenticator agree, so an abandoned page leaves
    // no half-configured account behind.
    let proposed_secret = auth::generate_totp_secret();
    let provisioning_uri =
        auth::totp_provisioning_uri("crm-light", &user.username, &proposed_secret);

    Ok(view! {
        <div class="page-head">
            <h1>"Your account"</h1>
        </div>

        <div class="cols">
            <div>
                <div class="panel">
                    <dl class="meta">
                        <dt>"Username"</dt>
                        <dd>(&user.username)</dd>
                        <dt>"Display name"</dt>
                        <dd>(user.display_name.as_deref().unwrap_or("—"))</dd>
                        <dt>"Role"</dt>
                        <dd>(domain::Role::from_stored(&user.role).label())</dd>
                        <dt>"Password"</dt>
                        <dd>
                            if auth::has_password(user.password_hash.as_deref()) {
                                "Set"
                            } else {
                                <strong class="warn-text">
                                    "Not set — anyone who can reach this port and knows the username can sign in."
                                </strong>
                            }
                        </dd>
                        <dt>"Two-factor"</dt>
                        <dd>
                            if two_factor_on { "Enabled" } else { "Not enabled" }
                        </dd>
                        <dt>"Last sign-in"</dt>
                        <dd>
                            (user.last_login_at
                                .map(|t| zone.format_datetime(t))
                                .unwrap_or_else(|| "—".to_string()))
                        </dd>
                        <dt>"Member since"</dt>
                        <dd>(zone.format_date(user.created_at))</dd>
                    </dl>
                </div>

                <h2>"Change password"</h2>
                <div class="panel">
                    <form method="post" action="/account/password">
                        csrf_field()
                        <div class="field">
                            <label for="current_password">"Current password"</label>
                            <input id="current_password" name="current_password" type="password"
                                   autocomplete="current-password">
                        </div>
                        <div class="field">
                            <label for="new_password">"New password"</label>
                            <input id="new_password" name="new_password" type="password"
                                   autocomplete="new-password">
                        </div>
                        <div class="field">
                            <label for="confirm_password">"Repeat new password"</label>
                            <input id="confirm_password" name="confirm_password" type="password"
                                   autocomplete="new-password">
                        </div>
                        <button class="btn btn-primary" type="submit">"Change password"</button>
                    </form>
                </div>

                <h2>"Two-factor authentication"</h2>
                <div class="panel">
                    if two_factor_on {
                        <p>"Two-factor is on. Turning it off requires your password."</p>
                        <form method="post" action="/account/totp/disable">
                            csrf_field()
                            <div class="field">
                                <label for="disable_password">"Password"</label>
                                <input id="disable_password" name="current_password" type="password"
                                       autocomplete="current-password">
                            </div>
                            <button class="btn btn-danger" type="submit">"Turn off two-factor"</button>
                        </form>
                    } else {
                        <p class="muted">
                            "Add the secret to your authenticator app, then confirm with a code it
                            generates."
                        </p>
                        <form method="post" action="/account/totp/enable">
                            csrf_field()
                            <div class="field">
                                <label for="totp_secret">"Secret"</label>
                                <input id="totp_secret" name="secret" value=(proposed_secret) readonly="">
                            </div>
                            <div class="field">
                                <label for="totp_code">"Code from the app"</label>
                                <input id="totp_code" name="code" inputmode="numeric"
                                       autocomplete="one-time-code">
                            </div>
                            <button class="btn btn-primary" type="submit">"Turn on two-factor"</button>
                        </form>
                        <p class="muted">"Or add it by hand: " (provisioning_uri)</p>
                    }
                </div>
            </div>

            <div>
                <h2>"Active sessions"</h2>
                <div class="panel">
                    if sessions.is_empty() {
                        <p class="empty">"No live sessions."</p>
                    } else {
                        for session in &sessions {
                            <div class="activity">
                                <div class="head">
                                    <span>(session.ip.as_deref().unwrap_or("address unknown"))</span>
                                    <span>(zone.format_datetime(session.created_at))</span>
                                </div>
                                <div class="body muted">
                                    (session.user_agent.as_deref().unwrap_or("client unknown"))
                                </div>
                            </div>
                        }
                    }
                </div>

                <form method="post" action="/account/sessions/revoke">
                    csrf_field()
                    <button class="btn btn-danger" type="submit">"Sign out everywhere else"</button>
                </form>
            </div>
        </div>
    })
}

#[derive(Deserialize)]
struct PasswordForm {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    confirm_password: String,
}

/// Check the password a sensitive form asked for.
///
/// Returns `true` for an account that has no password, where an empty field is
/// the only correct answer.
fn confirms_password(user: &User, supplied: &str) -> bool {
    match user.password_hash.as_deref() {
        Some(hash) if !hash.trim().is_empty() => auth::verify_password(supplied, hash),
        _ => supplied.is_empty(),
    }
}

#[route(POST "/account/password")]
async fn change_password(cx: &Cx, body: crate::csrf::CsrfForm<PasswordForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let crate::csrf::CsrfForm(form) = body;
    let current = auth::require_user(cx)?;
    let config = crate::config_of(cx);

    let mut user = User::filter(User::fields().id().eq(current.id))
        .first()
        .exec(&mut db)
        .await?
        .ok_or_not_found()?;

    // The current password is required even though the session already proves
    // identity: a borrowed laptop should not be enough to take an account over.
    if !confirms_password(&user, &form.current_password) {
        return Err(bad_request("That is not your current password.").into());
    }
    if form.new_password != form.confirm_password {
        return Err(bad_request("The two new passwords do not match.").into());
    }
    if let Some(problem) = auth::password_problem(&form.new_password) {
        return Err(bad_request(problem).into());
    }
    if form.new_password == form.current_password {
        return Err(bad_request("The new password is the same as the old one.").into());
    }

    let hash = auth::hash_password(&form.new_password)
        .map_err(anyhow::Error::msg)
        .map_err(internal_server_error)?;
    toasty::update!(user { password_hash: Some(hash) })
        .exec(&mut db)
        .await?;

    // Every other session dies with the old password; this one survives, so the
    // user is not signed out of the tab they are working in.
    let token = auth::session_token(cx);
    auth::revoke_user_sessions(&mut db, user.id, token.as_deref())
        .await
        .map_err(internal_server_error)?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        "Password changed. Every other session was signed out.",
    );
    Ok(see_other("/account"))
}

#[derive(Deserialize)]
struct TotpEnableForm {
    #[serde(default)]
    secret: String,
    #[serde(default)]
    code: String,
}

#[route(POST "/account/totp/enable")]
async fn enable_totp(cx: &Cx, body: crate::csrf::CsrfForm<TotpEnableForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let crate::csrf::CsrfForm(form) = body;
    let current = auth::require_user(cx)?;
    let config = crate::config_of(cx);

    if !auth::verify_totp(&form.secret, &form.code, domain::now()) {
        return Err(bad_request(
            "That code does not match the secret. Check your device's clock and try again.",
        )
        .into());
    }

    let mut user = User::filter(User::fields().id().eq(current.id))
        .first()
        .exec(&mut db)
        .await?
        .ok_or_not_found()?;
    toasty::update!(user { totp_secret: Some(form.secret.clone()) })
        .exec(&mut db)
        .await?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        "Two-factor authentication is on.",
    );
    Ok(see_other("/account"))
}

#[route(POST "/account/totp/disable")]
async fn disable_totp(cx: &Cx, body: crate::csrf::CsrfForm<PasswordForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let crate::csrf::CsrfForm(form) = body;
    let current = auth::require_user(cx)?;
    let config = crate::config_of(cx);

    let mut user = User::filter(User::fields().id().eq(current.id))
        .first()
        .exec(&mut db)
        .await?
        .ok_or_not_found()?;

    if !confirms_password(&user, &form.current_password) {
        return Err(bad_request("That is not your password.").into());
    }

    toasty::update!(user { totp_secret: None }).exec(&mut db).await?;
    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        "Two-factor authentication is off.",
    );
    Ok(see_other("/account"))
}

#[route(POST "/account/sessions/revoke")]
async fn revoke_other_sessions(cx: &Cx) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let current = auth::require_user(cx)?;
    let config = crate::config_of(cx);

    let token = auth::session_token(cx);
    let revoked = auth::revoke_user_sessions(&mut db, current.id, token.as_deref())
        .await
        .map_err(internal_server_error)?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!("Signed out {revoked} other session(s)."),
    );
    Ok(see_other("/account"))
}

/// Render a duration in seconds as something a person would write.
fn humanize_seconds(seconds: i64) -> String {
    if seconds < 90 {
        format!("{seconds} seconds")
    } else if seconds < 5400 {
        format!("{} minutes", (seconds + 59) / 60)
    } else {
        format!("{} hours", (seconds + 3599) / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landing_targets_stay_on_this_origin() {
        assert_eq!(landing(Some("/deals?q=x")), "/deals?q=x");
        assert_eq!(landing(Some("https://evil.example")), "/");
        assert_eq!(landing(None), "/");
        assert_eq!(landing(Some("")), "/");
    }

    #[test]
    fn durations_read_like_english() {
        assert_eq!(humanize_seconds(30), "30 seconds");
        assert_eq!(humanize_seconds(89), "89 seconds");
        assert_eq!(humanize_seconds(900), "15 minutes");
        assert_eq!(humanize_seconds(7200), "2 hours");
    }
}
