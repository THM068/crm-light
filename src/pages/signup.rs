//! Sign-up: create a workspace and its first administrator.
//!
//! This is the only way into a fresh installation. The person who signs up
//! becomes the administrator of a new [`Account`](crate::models::Account) and
//! can then add colleagues at `/admin/users`; everybody in one workspace sees
//! that workspace's records and nobody else's.
//!
//! # Why sign-up asks for a workspace name
//!
//! Sign-in needs to know *which* workspace a username belongs to, because
//! usernames are only unique within one — two organisations may each have their
//! own `admin`. Rather than asking for a workspace at sign-in time (which is a
//! strange thing to ask somebody who is just trying to log in), the sign-in form
//! asks for the workspace's **slug**, and the slug is derived from the name
//! given here. The rule is stated on both forms so nobody has to guess.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{SeeOther, bad_request, internal_server_error, not_found, see_other},
        page, route,
    },
    view::{View, view},
};

use crate::access;
use crate::auth;
use crate::config_of;
use crate::domain::{self, Role};
use crate::flash;
use crate::models::{Account, User};
use crate::views::{csrf_field, flash_banner};

/// Whether this installation accepts new workspaces.
///
/// Read on every request rather than captured at startup, so the decision lives
/// in one place (`Config::allow_signup`) and both the page and the handler obey
/// it.
fn signup_allowed(cx: &Cx) -> bool {
    config_of(cx).allow_signup
}

#[page("/signup")]
async fn signup_form(cx: &Cx) -> Result<impl View> {
    if !signup_allowed(cx) {
        // A 404 rather than a 403: an installation that does not take sign-ups
        // should not advertise that it might.
        return Err(not_found().into());
    }

    Ok(view! {
        <div class="auth-card">
            <h1>"Create a workspace"</h1>
            <p class="muted">
                "You will be its administrator. Add the rest of your team once you are in."
            </p>

            flash_banner(
                kind: "warn",
                message: "This installation has no accounts yet, or you are starting a second \
                          workspace. Either way, sign-up creates a new one with you as its \
                          administrator — it does not join an existing workspace.",
            )

            <form method="post" action="/signup">
                csrf_field()
                <div class="field">
                    <label for="account_name">"Workspace name"</label>
                    <input id="account_name" name="account_name" autofocus="" required=""
                           placeholder="Acme Robotics">
                    <span class="hint">
                        "Your team signs in with the slug this produces, e.g. " <code>"acme-robotics"</code>
                    </span>
                </div>
                <div class="field">
                    <label for="username">"Your username"</label>
                    <input id="username" name="username" autocomplete="username" required="">
                    <span class="hint">"Unique inside your workspace, not across the site."</span>
                </div>
                <div class="field">
                    <label for="display_name">"Your name"</label>
                    <input id="display_name" name="display_name" autocomplete="name">
                </div>
                <div class="field">
                    <label for="password">"Password"</label>
                    <input id="password" name="password" type="password"
                           autocomplete="new-password" required="">
                    <span class="hint">
                        "At least " (auth::MIN_PASSWORD_LEN) " characters; a few words in a row is fine."
                    </span>
                </div>
                <div class="field">
                    <label for="confirm_password">"Repeat password"</label>
                    <input id="confirm_password" name="confirm_password" type="password"
                           autocomplete="new-password" required="">
                </div>
                <button class="btn btn-primary" type="submit">"Create workspace"</button>
            </form>

            <p class="muted">"Already have an account? " <a href="/login">"Sign in"</a></p>
        </div>
    })
}

#[derive(Deserialize)]
pub struct SignupForm {
    #[serde(default)]
    pub account_name: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub confirm_password: String,
}

#[route(POST "/signup")]
async fn signup(cx: &Cx, body: crate::csrf::CsrfForm<SignupForm>) -> Result<SeeOther> {
    let crate::csrf::CsrfForm(form) = body;
    if !signup_allowed(cx) {
        return Err(not_found().into());
    }
    let config = config_of(cx);
    let mut db = crate::db(cx);

    let (account_name, slug) = access::check_account_name(&form.account_name)?;
    let (username, username_lower) = access::check_username(&form.username)?;

    // The slug is how sign-in finds this workspace, so it has to be unique.
    if Account::filter(Account::fields().slug().eq(slug.clone()))
        .first()
        .exec(&mut db)
        .await?
        .is_some()
    {
        return Err(bad_request(format!(
            "The workspace name {account_name:?} produces the sign-in name {slug:?}, which is \
             already taken. Try a name that is distinguishable."
        ))
        .into());
    }

    if form.password != form.confirm_password {
        return Err(bad_request("The two passwords do not match.").into());
    }
    if let Some(problem) = auth::password_problem(&form.password) {
        return Err(bad_request(problem).into());
    }

    let now = domain::now();

    let account = toasty::create!(Account {
        name: account_name,
        slug: slug.clone(),
        created_at: now,
        created_by: None,
    })
    .exec(&mut db)
    .await?;

    let hash = auth::hash_password(&form.password)
        .map_err(anyhow::Error::msg)
        .map_err(internal_server_error)?;

    // The first member of a workspace is its administrator; that is the whole
    // point of signing up rather than being added.
    let user = toasty::create!(User {
        account_id: account.id,
        username,
        username_lower,
        display_name: domain::opt(form.display_name),
        password_hash: Some(hash),
        role: Role::Admin.as_str(),
        totp_secret: None,
        active: true,
        created_at: now,
        last_login_at: Some(now),
    })
    .exec(&mut db)
    .await?;

    // Record who founded it, now that the user exists.
    let mut account = account;
    toasty::update!(account { created_by: Some(user.id) })
        .exec(&mut db)
        .await?;

    // Sign them straight in: making somebody who just chose a password type it
    // again is a pointless round trip.
    let user_agent = topcoat::router::request::headers(cx)
        .get(topcoat::router::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let ip = topcoat::router::request::headers(cx)
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string());

    let token = auth::create_session(
        &mut db,
        config,
        user.id,
        account.id,
        user_agent.as_deref(),
        ip.as_deref(),
    )
    .await
    .map_err(internal_server_error)?;

    auth::set_cookie_header(cx, auth::session_cookie(token, config, &slug));

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!(
            "Welcome. You are the administrator of {}. Add your team from Accounts.",
            slug
        ),
    );

    Ok(see_other("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_slug_is_derived_from_its_name() {
        let (name, slug) = access::check_account_name("Acme Robotics").expect("valid");
        assert_eq!(name, "Acme Robotics");
        assert_eq!(slug, "acme-robotics");
    }

    #[test]
    fn a_name_with_nothing_usable_is_refused() {
        // Refusing here beats creating a workspace nobody can sign in to,
        // because the slug is half of the sign-in credential.
        assert!(access::check_account_name("!!!").is_err());
        assert!(access::check_account_name("---").is_err());
    }
}
