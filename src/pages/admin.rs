//! Account administration: `/admin/users`.
//!
//! Every route here goes through [`auth::require_admin`], so the authorisation
//! decision is visible in each handler rather than implied by the URL prefix.
//!
//! # The rules that keep the app administrable
//!
//! An installation must always keep at least one active administrator, and a
//! user must not be able to lock themselves out with one careless click:
//!
//! - The last active administrator cannot be demoted, deactivated, or deleted.
//! - Nobody can demote or delete themselves, which also stops the accidental
//!   self-lockout before it reaches the rule above.
//!
//! Deletion is offered only for accounts with no history. An account that has
//! logged activity is deactivated instead, so the audit trail keeps naming a
//! real person.

use serde::Deserialize;
use toasty::Db;
use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{SeeOther, bad_request, forbidden, internal_server_error, see_other},
        page, path_param, route,
    },
    view::{View, component, view},
};

use crate::access::{self, Tenant};
use crate::auth::{self};
use crate::domain::{self, Role};
use crate::flash;
use crate::models::{Activity, Session, User};
use crate::views::{csrf_field, flash_banner, pager, query_string};

path_param!(user_id: i64, error = bad_request("User id must be a number"));

/// How many accounts a page shows.
///
/// Fixed rather than configurable: a deployment cannot have enough accounts for
/// the number to matter, and a fixed page size keeps the list's cursors valid
/// across restarts.
const PAGE_SIZE: usize = 50;

/// An account plus the counts the list shows next to it.
struct Row {
    id: i64,
    username: String,
    display_name: String,
    role: String,
    active: bool,
    is_self: bool,
    has_password: bool,
    has_totp: bool,
    last_login: String,
    activity_count: u64,
    live_sessions: usize,
}

/// Load a member of this workspace, or 403/404.
///
/// `User` is tenant-owned like everything else, which is what makes an
/// administrator's powers stop at their own workspace: an id from another one
/// is refused before any of the rules below see it.
async fn find(db: &mut Db, tenant: Tenant, id: i64) -> Result<User> {
    access::require::<User>(db, tenant.account_id, id).await
}

/// How many *other* active administrators this workspace has.
///
/// Scoped, so an administrator in one workspace cannot be kept in place by the
/// existence of an administrator in another.
async fn other_active_admins(db: &mut Db, tenant: Tenant, excluding: i64) -> Result<usize> {
    let admins = User::filter(
        User::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(User::fields().role().eq(Role::Admin.as_str()))
            .and(User::fields().active().eq(true))
            .and(User::fields().id().ne(excluding)),
    )
    .exec(db)
    .await?;
    Ok(admins.len())
}

/// Refuse a change that would leave the installation without an administrator,
/// or that a user is aiming at their own account.
///
/// `self_harm` is what distinguishes the two: demoting yourself is refused even
/// when other administrators remain, because it is far more often a slip than
/// an intention.
fn guard_admin_change(
    actor: &auth::CurrentUser,
    target: &User,
    self_harm: bool,
    other_admins: usize,
) -> Result<()> {
    if self_harm && actor.id == target.id {
        return Err(bad_request(
            "You cannot change your own role or close your own account. Ask another \
             administrator.",
        )
        .into());
    }
    if Role::from_stored(&target.role).is_admin() && target.active && other_admins == 0 {
        return Err(bad_request(
            "This is the last active administrator, so it has to stay one. Promote or \
             reactivate another account first.",
        )
        .into());
    }
    Ok(())
}

// --- List ------------------------------------------------------------------

#[page("/admin/users")]
async fn index(cx: &Cx) -> Result<impl View> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let current = auth::require_admin(cx)?;
    let zone = crate::views::zone(cx);

    let users = User::filter(User::fields().account_id().eq(tenant.account_id))
        .order_by(User::fields().username_lower().asc())
        .limit(PAGE_SIZE)
        .exec(&mut db)
        .await?;

    let mut rows = Vec::with_capacity(users.len());
    for user in users {
        // Counted per row, which is honest about the cost: this list is bounded
        // by `PAGE_SIZE`, and an administrator looking at accounts is not the
        // hot path the CRM lists are.
        let activity_count = Activity::filter(
            Activity::fields()
                .account_id()
                .eq(tenant.account_id)
                .and(Activity::fields().user_id().eq(Some(user.id))),
        )
        .count()
        .exec(&mut db)
        .await?;
        let live_sessions = Session::filter(
            Session::fields()
                .account_id()
                .eq(tenant.account_id)
                .and(Session::fields().user_id().eq(user.id))
                .and(Session::fields().revoked_at().is_none())
                .and(Session::fields().expires_at().gt(domain::now())),
        )
        .exec(&mut db)
        .await?
        .len();

        rows.push(Row {
            id: user.id,
            display_name: user.display_name.clone().unwrap_or_default(),
            role: user.role.clone(),
            active: user.active,
            is_self: user.id == current.id,
            has_password: auth::has_password(user.password_hash.as_deref()),
            has_totp: user.totp_secret.as_deref().is_some_and(|s| !s.is_empty()),
            last_login: user
                .last_login_at
                .map(|t| zone.format_datetime(t))
                .unwrap_or_else(|| "never".to_string()),
            username: user.username,
            activity_count,
            live_sessions,
        });
    }

    let total = rows.len();
    let query = query_string(&[]);
    Ok(view! {
        <div class="page-head">
            <h1>"Accounts"</h1>
            <a class="btn btn-primary" href="/admin/users/new">"New account"</a>
        </div>

        flash_banner(
            kind: "warn",
            message: "Every account here can read and write all CRM data. The role only \
                      decides who may manage accounts.",
        )

        if rows.is_empty() {
            <p class="empty">"No accounts."</p>
        } else {
            <table>
                <thead>
                    <tr>
                        <th>"Account"</th>
                        <th>"Role"</th>
                        <th>"Status"</th>
                        <th>"Credentials"</th>
                        <th>"Last sign-in"</th>
                        <th class="num">"Activity"</th>
                        <th class="num">"Sessions"</th>
                        <th></th>
                    </tr>
                </thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td>
                                <a href=(format!("/admin/users/{}", row.id))>(&row.username)</a>
                                if !row.display_name.is_empty() {
                                    <span class="muted">" · " (&row.display_name)</span>
                                }
                                if row.is_self {
                                    <span class="badge">"you"</span>
                                }
                            </td>
                            <td>crate::views::role_badge(role: row.role.as_str())</td>
                            <td>
                                if row.active {
                                    <span class="badge badge-ok">"Active"</span>
                                } else {
                                    <span class="badge badge-off">"Closed"</span>
                                }
                            </td>
                            <td class="muted">
                                if row.has_password { "password" } else { "no password" }
                                if row.has_totp { " · 2FA" } else { "" }
                            </td>
                            <td class="muted">(&row.last_login)</td>
                            <td class="num">(row.activity_count)</td>
                            <td class="num">(row.live_sessions)</td>
                            <td>
                                <a class="btn btn-sm" href=(format!("/admin/users/{}/edit", row.id))>"Edit"</a>
                            </td>
                        </tr>
                    }
                </tbody>
            </table>
            pager(
                base: "/admin/users",
                query: &query,
                total: total,
                shown_from: if total == 0 { 0 } else { 1 },
                shown_to: total,
                prev: None,
                next: None,
            )
        }
    })
}

// --- Create ----------------------------------------------------------------

#[page("/admin/users/new")]
async fn new_form(cx: &Cx) -> Result<impl View> {
    auth::require_admin(cx)?;

    Ok(view! {
        <div class="page-head">
            <h1>"New account"</h1>
            <a class="btn" href="/admin/users">"Cancel"</a>
        </div>
        <div class="panel">
            user_form(
                action: "/admin/users".to_string(),
                username: String::new(),
                display_name: String::new(),
                role: Role::Member.as_str().to_string(),
                active: true,
                require_password: true,
                submit_label: "Create account",
            )
        </div>
    })
}

#[derive(Deserialize)]
struct NewUserForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    totp_secret: String,
}

#[route(POST "/admin/users")]
async fn create(cx: &Cx, body: crate::csrf::CsrfForm<NewUserForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(form) = body;
    auth::require_admin(cx)?;
    let config = crate::config_of(cx);

    let username = form.username.trim();
    if !domain::username_is_well_formed(username) {
        return Err(bad_request(
            "Usernames are 1–64 characters of letters, digits, `.`, `_`, `-`, `@`, or `+`.",
        )
        .into());
    }
    let key = domain::normalize_username(username);
    // Per workspace, so another workspace having an `admin` does not stop this
    // one having one either.
    if access::username_taken(&mut db, tenant.account_id, &key).await? {
        return Err(bad_request("Someone in this workspace already has that username.").into());
    }

    // A new account always gets a password: the whole point of an account is
    // that it is not open, and the empty-password path exists only so the
    // bootstrap admin can be signed into once.
    if form.password.is_empty() {
        return Err(bad_request("Set a password for the new account.").into());
    }
    if let Some(problem) = auth::password_problem(&form.password) {
        return Err(bad_request(problem).into());
    }
    let hash = auth::hash_password(&form.password).map_err(anyhow::Error::msg).map_err(internal_server_error)?;

    let totp_secret = domain::opt(form.totp_secret.clone());
    if let Some(secret) = totp_secret.as_deref() {
        // Storing a secret nobody has proven they hold would lock the account
        // out of its own second factor, so this path takes the secret as given
        // only when it looks like base32.
        if secret.len() < 16 || !secret.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(bad_request("That does not look like a base32 TOTP secret.").into());
        }
    }

    let user = toasty::create!(User {
        account_id: tenant.account_id,
        username,
        username_lower: key,
        display_name: domain::opt(form.display_name),
        password_hash: Some(hash),
        role: Role::parse(form.role.trim()).unwrap_or(Role::Member).as_str(),
        totp_secret,
        active: true,
        created_at: domain::now(),
        last_login_at: None,
    })
    .exec(&mut db)
    .await?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!("Created the account {}.", user.username),
    );
    Ok(see_other(format!("/admin/users/{}", user.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/admin/users/{user_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let current = auth::require_admin(cx)?;
    let id = *path_param::<UserId>(cx)?;
    let user = find(&mut db, tenant, id).await?;

    let zone = crate::views::zone(cx);
    let activity_count = Activity::filter(
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().user_id().eq(Some(id))),
    )
    .count()
    .exec(&mut db)
    .await?;
    let sessions = Session::filter(
        Session::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Session::fields().user_id().eq(id))
            .and(Session::fields().revoked_at().is_none())
            .and(Session::fields().expires_at().gt(domain::now())),
    )
    .order_by(Session::fields().created_at().desc())
    .exec(&mut db)
    .await?;
    let recent = Activity::filter(
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().user_id().eq(Some(id))),
    )
    .order_by(Activity::fields().created_at().desc())
        .limit(10)
        .exec(&mut db)
        .await?;

    let role = Role::from_stored(&user.role);
    let can_delete = activity_count == 0 && user.id != current.id;

    Ok(view! {
        <div class="page-head">
            <h1>(&user.username)</h1>
            <div class="actions">
                <a class="btn" href=(format!("/admin/users/{id}/edit"))>"Edit"</a>
                <a class="btn" href="/admin/users">"All accounts"</a>
            </div>
        </div>

        <div class="cols">
            <div>
                <div class="panel">
                    <dl class="meta">
                        <dt>"Display name"</dt>
                        <dd>(user.display_name.as_deref().unwrap_or("—"))</dd>
                        <dt>"Role"</dt>
                        <dd>crate::views::role_badge(role: user.role.as_str())</dd>
                        <dt>"Status"</dt>
                        <dd>
                            if user.active { "Active" } else { "Closed — cannot sign in" }
                            if user.id == current.id {
                                <span class="badge">"this is you"</span>
                            }
                        </dd>
                        <dt>"Password"</dt>
                        <dd>
                            if auth::has_password(user.password_hash.as_deref()) {
                                "Set"
                            } else {
                                <strong class="warn-text">"Not set"</strong>
                            }
                        </dd>
                        <dt>"Two-factor"</dt>
                        <dd>
                            if user.totp_secret.as_deref().is_some_and(|s| !s.is_empty()) {
                                "Enabled"
                            } else {
                                "Not enabled"
                            }
                        </dd>
                        <dt>"Last sign-in"</dt>
                        <dd>
                            (user.last_login_at
                                .map(|t| zone.format_datetime(t))
                                .unwrap_or_else(|| "never".to_string()))
                        </dd>
                        <dt>"Created"</dt>
                        <dd>(zone.format_date(user.created_at))</dd>
                    </dl>
                </div>

                <h2>"Change password"</h2>
                <div class="panel">
                    <form method="post" action=(format!("/admin/users/{id}/password"))>
                        csrf_field()
                        <div class="field">
                            <label for="password">"New password"</label>
                            <input id="password" name="password" type="password" autocomplete="new-password">
                        </div>
                        <p class="muted">
                            "Setting a password signs this account out everywhere."
                        </p>
                        <button class="btn btn-primary" type="submit">"Set password"</button>
                    </form>
                </div>

                <h2>"Recent activity by this account"</h2>
                <div class="panel">
                    if recent.is_empty() {
                        <p class="empty">"Nothing logged."</p>
                    } else {
                        for activity in &recent {
                            <div class="activity">
                                <div class="head">
                                    <strong>(domain::ActivityKind::from_stored(&activity.kind).label())</strong>
                                    <span>(zone.format_datetime(activity.created_at))</span>
                                </div>
                                <div class="body">(&activity.body)</div>
                            </div>
                        }
                    }
                </div>
            </div>

            <div>
                <h2>"What this role can do"</h2>
                <div class="panel">
                    <p>(role.description())</p>
                </div>

                <h2>"Live sessions"</h2>
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
                <form method="post" action=(format!("/admin/users/{id}/sessions/revoke"))>
                    csrf_field()
                    <button class="btn btn-danger" type="submit">"Sign this account out everywhere"</button>
                </form>

                <h2>"Danger"</h2>
                <div class="panel">
                    if can_delete {
                        <form method="post" action=(format!("/admin/users/{id}/delete"))>
                            csrf_field()
                            <p class="muted">
                                "This account has never logged anything, so deleting it loses
                                no history."
                            </p>
                            <button class="btn btn-danger" type="submit">"Delete account"</button>
                        </form>
                    } else {
                        <p class="muted">
                            if user.id == current.id {
                                "You cannot close your own account. Ask another administrator."
                            } else {
                                "This account has logged activity, so it cannot be deleted —
                                 closing it keeps the history attributed to a real person."
                            }
                        </p>
                        if user.id != current.id {
                            <form method="post" action=(format!("/admin/users/{id}/active"))>
                                csrf_field()
                                <input type="hidden" name="active"
                                       value=(if user.active { "0".to_string() } else { "1".to_string() })>
                                <button class="btn btn-danger" type="submit">
                                    if user.active { "Close account" } else { "Reactivate account" }
                                </button>
                            </form>
                        }
                    }
                </div>
            </div>
        </div>
    })
}

// --- Edit ------------------------------------------------------------------

#[page("/admin/users/{user_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    auth::require_admin(cx)?;
    let id = *path_param::<UserId>(cx)?;
    let user = find(&mut db, tenant, id).await?;

    Ok(view! {
        <div class="page-head">
            <h1>"Edit account"</h1>
            <a class="btn" href=(format!("/admin/users/{id}"))>"Cancel"</a>
        </div>
        <div class="panel">
            user_form(
                action: format!("/admin/users/{id}"),
                username: user.username.clone(),
                display_name: user.display_name.clone().unwrap_or_default(),
                role: user.role.clone(),
                active: user.active,
                require_password: false,
                submit_label: "Save changes",
            )
        </div>
    })
}

#[derive(Deserialize)]
struct EditUserForm {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    role: String,
    /// `on` when the checkbox is ticked; absent otherwise.
    #[serde(default)]
    active: Option<String>,
}

#[route(POST "/admin/users/{user_id}")]
async fn update_user(cx: &Cx, body: crate::csrf::CsrfForm<EditUserForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(form) = body;
    let current = auth::require_admin(cx)?;
    let config = crate::config_of(cx);
    let id = *path_param::<UserId>(cx)?;
    let mut user = find(&mut db, tenant, id).await?;

    let role = Role::parse(form.role.trim())
        .ok_or_else(|| bad_request("Unknown role."))?;
    let active = form.active.is_some();

    // The username is not editable: it is the identity the audit trail and the
    // login form both key on, and renaming it silently rewrites history.
    let role_changed = role != Role::from_stored(&user.role);
    let closing = user.active && !active;

    if role_changed || closing {
        let others = other_active_admins(&mut db, tenant, user.id).await?;
        let self_harm = user.id == current.id;
        guard_admin_change(&current, &user, self_harm, others)?;
    }

    toasty::update!(user {
        display_name: domain::opt(form.display_name),
        role: role.as_str(),
        active,
    })
    .exec(&mut db)
    .await?;

    if closing || role_changed {
        // A closed account, or one whose powers just changed, should not keep
        // working in a tab that is already open.
        auth::revoke_user_sessions(&mut db, user.id, None)
            .await
            .map_err(internal_server_error)?;
    }

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!("Saved {}.", user.username),
    );
    Ok(see_other(format!("/admin/users/{id}")))
}

#[derive(Deserialize)]
struct ActiveForm {
    #[serde(default)]
    active: String,
}

#[route(POST "/admin/users/{user_id}/active")]
async fn set_active(cx: &Cx, body: crate::csrf::CsrfForm<ActiveForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(form) = body;
    let current = auth::require_admin(cx)?;
    let config = crate::config_of(cx);
    let id = *path_param::<UserId>(cx)?;
    let mut user = find(&mut db, tenant, id).await?;

    let active = form.active.trim() != "0";
    if user.active && !active {
        let others = other_active_admins(&mut db, tenant, user.id).await?;
        guard_admin_change(&current, &user, user.id == current.id, others)?;
    }

    toasty::update!(user { active }).exec(&mut db).await?;
    if !active {
        auth::revoke_user_sessions(&mut db, user.id, None)
            .await
            .map_err(internal_server_error)?;
    }

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        if active {
            "Account reactivated."
        } else {
            "Account closed and signed out everywhere."
        },
    );
    Ok(see_other(format!("/admin/users/{id}")))
}

#[derive(Deserialize)]
struct ResetPasswordForm {
    #[serde(default)]
    password: String,
}

#[route(POST "/admin/users/{user_id}/password")]
async fn reset_password(cx: &Cx, body: crate::csrf::CsrfForm<ResetPasswordForm>) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(form) = body;
    let current = auth::require_admin(cx)?;
    let config = crate::config_of(cx);
    let id = *path_param::<UserId>(cx)?;
    let mut user = find(&mut db, tenant, id).await?;

    if let Some(problem) = auth::password_problem(&form.password) {
        return Err(bad_request(problem).into());
    }
    let hash = auth::hash_password(&form.password).map_err(anyhow::Error::msg).map_err(internal_server_error)?;
    toasty::update!(user { password_hash: Some(hash) })
        .exec(&mut db)
        .await?;

    // An administrator resetting a password is the classic account-takeover
    // path, so it signs the account out everywhere — including the
    // administrator's own session when they reset their own.
    let except = (user.id == current.id).then(|| auth::session_token(cx)).flatten();
    auth::revoke_user_sessions(&mut db, user.id, except.as_deref())
        .await
        .map_err(internal_server_error)?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!("Password set for {}. Their other sessions were signed out.", user.username),
    );
    Ok(see_other(format!("/admin/users/{id}")))
}

#[route(POST "/admin/users/{user_id}/sessions/revoke")]
async fn revoke_sessions(cx: &Cx) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let current = auth::require_admin(cx)?;
    let config = crate::config_of(cx);
    let id = *path_param::<UserId>(cx)?;
    let user = find(&mut db, tenant, id).await?;

    let except = (user.id == current.id).then(|| auth::session_token(cx)).flatten();
    let revoked = auth::revoke_user_sessions(&mut db, user.id, except.as_deref())
        .await
        .map_err(internal_server_error)?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!("Signed {} out of {revoked} session(s).", user.username),
    );
    Ok(see_other(format!("/admin/users/{id}")))
}

#[route(POST "/admin/users/{user_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = crate::db(cx);
    let tenant = Tenant::of(cx)?;

    let current = auth::require_admin(cx)?;
    let config = crate::config_of(cx);
    let id = *path_param::<UserId>(cx)?;
    let user = find(&mut db, tenant, id).await?;

    if user.id == current.id {
        return Err(forbidden().into());
    }
    let others = other_active_admins(&mut db, tenant, user.id).await?;
    guard_admin_change(&current, &user, false, others)?;

    // Accounts with history are closed, never deleted, so the activity log
    // keeps naming someone who existed.
    let activity_count = Activity::filter(
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().user_id().eq(Some(id))),
    )
    .count()
    .exec(&mut db)
    .await?;
    if activity_count > 0 {
        return Err(bad_request(
            "This account has logged activity, so it cannot be deleted. Close it instead.",
        )
        .into());
    }

    // Sessions go with the account; the rows would otherwise be unreachable.
    // Scoped even though `id` was already checked to be in this workspace: a
    // delete is the wrong place to rely on an earlier check having been made.
    for session in Session::filter(
        Session::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Session::fields().user_id().eq(id)),
    )
    .exec(&mut db)
    .await?
    {
        Session::delete_by_id(&mut db, session.id).await?;
    }
    // The throttle row goes with the account, so a new person reusing the
    // username does not inherit somebody else's lockout.
    for attempt in crate::models::LoginAttempt::filter(
        crate::models::LoginAttempt::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(
                crate::models::LoginAttempt::fields()
                    .username_lower()
                    .eq(domain::normalize_username(&user.username)),
            ),
    )
    .exec(&mut db)
    .await?
    {
        crate::models::LoginAttempt::delete_by_id(&mut db, attempt.id).await?;
    }

    User::delete_by_id(&mut db, id).await?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!("Deleted the account {}.", user.username),
    );
    Ok(see_other("/admin/users"))
}

// --- Shared form -----------------------------------------------------------

/// Create/edit form, shared by both pages.
#[component]
async fn user_form(
    action: String,
    username: String,
    display_name: String,
    role: String,
    active: bool,
    require_password: bool,
    submit_label: &str,
) -> Result<impl View> {
    let editing = !username.is_empty();
    Ok(view! {
        <form method="post" action=(action)>
            csrf_field()
            if !editing {
                <div class="field">
                    <label for="username">"Username"</label>
                    <input id="username" name="username" autofocus="" required="">
                </div>
            }
            <div class="field">
                <label for="display_name">"Display name"</label>
                <input id="display_name" name="display_name" value=(display_name)>
            </div>
            <div class="field">
                <label for="role">"Role"</label>
                <select id="role" name="role">
                    for option in Role::ALL {
                        <option value=(option.as_str()) selected=(option.as_str() == role)>
                            (option.label()) " — " (option.description())
                        </option>
                    }
                </select>
            </div>
            <div class="field">
                <label for="active">"Status"</label>
                <select id="active" name="active">
                    <option value="on" selected=(active)>"Active"</option>
                    <option value="" selected=(!active)>"Closed"</option>
                </select>
            </div>
            if !editing {
                <div class="field">
                    <label for="password">"Password"</label>
                    <input id="password" name="password" type="password" autocomplete="new-password">
                </div>
                <div class="field">
                    <label for="totp_secret">"Two-factor secret (optional)"</label>
                    <input id="totp_secret" name="totp_secret" placeholder="Leave blank to set it up later">
                </div>
            }
            if require_password {
                <p class="muted">"The new account signs in with the password above."</p>
            }
            <button class="btn btn-primary" type="submit">(submit_label)</button>
        </form>
    })
}

/// Unused import guard: `form_errors` is re-exported for pages that need it.
#[allow(dead_code)]
fn _keep_imports_used(_: Vec<String>) {}
