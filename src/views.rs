//! Shared view components.
//!
//! These are the pieces more than one page renders: the stage badge, the
//! activity feed and its form, the CSRF field every form carries, the pager,
//! and the flash banner.

use std::collections::HashMap;

use topcoat::{
    Result,
    context::{Cx, app_context},
    view::{View, component, view},
};

use crate::auth;
use crate::csrf;
use crate::domain::{self, ActivityKind, Role, Stage, TimeZone};
use crate::models::{Activity, User};

/// Render an optional id for a hidden form input; absent becomes empty.
fn id_value(id: Option<i64>) -> String {
    id.map(|id| id.to_string()).unwrap_or_default()
}

// --- CSRF ------------------------------------------------------------------

/// The hidden field that carries the CSRF token.
///
/// Reads the token the guard put in the request context; renders an empty value
/// outside a guarded request, which then fails validation rather than silently
/// passing.
#[component]
pub async fn csrf_field(cx: &Cx) -> Result<impl View> {
    Ok(view! {
        <input type="hidden" name=(csrf::FIELD) value=(csrf::field_value(cx))>
    })
}

// --- Chrome ----------------------------------------------------------------

/// A one-shot notice: what the last request did, or why it failed.
#[component]
pub async fn flash_banner(kind: &str, message: &str) -> Result<impl View> {
    let class = match kind {
        "error" => "flash flash-error",
        "warn" => "flash flash-warn",
        _ => "flash flash-ok",
    };
    Ok(view! {
        <div class=(class) role="status">(message)</div>
    })
}

/// A list of validation failures, for a form being re-rendered.
#[component]
pub async fn form_errors(errors: Vec<String>) -> Result<impl View> {
    Ok(view! {
        if !errors.is_empty() {
            <div class="flash flash-error" role="alert">
                <ul>
                    for error in &errors {
                        <li>(error)</li>
                    }
                </ul>
            </div>
        }
    })
}

// --- CRM widgets -----------------------------------------------------------

/// Coloured badge showing a deal's stage.
#[component]
pub async fn stage_badge(stage: &str) -> Result<impl View> {
    let stage = Stage::from_stored(stage);
    Ok(view! {
        <span class=(format!("badge {}", stage.css_class()))>(stage.label())</span>
    })
}

/// Badge showing a user's role.
#[component]
pub async fn role_badge(role: &str) -> Result<impl View> {
    let role = Role::from_stored(role);
    let class = if role.is_admin() {
        "badge badge-admin"
    } else {
        "badge"
    };
    Ok(view! {
        <span class=(class)>(role.label())</span>
    })
}

/// A chronological list of logged activities, newest first.
///
/// Each entry names whoever logged it. The names are resolved by the caller,
/// which holds the database handle, and passed in as a map so this component
/// stays a pure render.
#[component]
pub async fn activity_feed(
    activities: Vec<Activity>,
    authors: HashMap<i64, String>,
    zone: TimeZone,
) -> Result<impl View> {
    Ok(view! {
        if activities.is_empty() {
            <p class="empty">"Nothing logged yet."</p>
        } else {
            for activity in activities {
                <div class="activity">
                    <div class="head">
                        <strong>(ActivityKind::from_stored(&activity.kind).label())</strong>
                        <span class="actions">
                            <span>(zone.format_datetime(activity.created_at))</span>
                            <span class="muted">
                                (activity.user_id
                                    .and_then(|id| authors.get(&id).cloned())
                                    .unwrap_or_else(|| "—".to_string()))
                            </span>
                            <form class="inline" method="post"
                                  action=(format!("/activities/{}/delete", activity.id))>
                                csrf_field()
                                <button class="btn btn-sm btn-danger" type="submit">"Delete"</button>
                            </form>
                        </span>
                    </div>
                    <div class="body">(&activity.body)</div>
                </div>
            }
        }
    })
}

/// Resolve the display names for the authors of `activities`.
///
/// One query for the whole page rather than one per activity.
pub async fn author_names(
    db: &mut toasty::Db,
    activities: &[Activity],
) -> toasty::Result<HashMap<i64, String>> {
    let mut ids: Vec<i64> = activities.iter().filter_map(|activity| activity.user_id).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }

    let users = User::filter(User::fields().id().in_list(ids))
        .exec(db)
        .await?;
    Ok(users
        .into_iter()
        .map(|user| {
            let name = user
                .display_name
                .clone()
                .unwrap_or_else(|| user.username.clone());
            (user.id, name)
        })
        .collect())
}

/// Inline form for logging an activity against a company, contact, or deal.
///
/// All three ids are submitted; the ones that do not apply are sent empty and
/// read back as `None`.
#[component]
pub async fn activity_form(
    action: &str,
    company_id: Option<i64>,
    contact_id: Option<i64>,
    deal_id: Option<i64>,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
            csrf_field()
            <input type="hidden" name="company_id" value=(id_value(company_id))>
            <input type="hidden" name="contact_id" value=(id_value(contact_id))>
            <input type="hidden" name="deal_id" value=(id_value(deal_id))>
            <div class="form-row">
                <div class="field">
                    <label for="kind">"Type"</label>
                    <select id="kind" name="kind">
                        for kind in ActivityKind::ALL {
                            <option value=(kind.as_str())>(kind.label())</option>
                        }
                    </select>
                </div>
            </div>
            <div class="field">
                <label for="body">"Note"</label>
                <textarea id="body" name="body" required="" placeholder="What happened?"></textarea>
            </div>
            <button class="btn btn-primary" type="submit">"Log activity"</button>
        </form>
    })
}

// --- Pagination ------------------------------------------------------------

/// Previous/next links plus a "showing x–y of z" summary.
///
/// Takes the four link-and-range values rather than a `Page<_>`, because a view
/// component cannot be generic over the row type.
#[component]
pub async fn pager(
    base: &str,
    query: &str,
    total: usize,
    shown_from: usize,
    shown_to: usize,
    prev: Option<String>,
    next: Option<String>,
) -> Result<impl View> {
    let prev = prev.map(|cursor| cursor_link(base, query, "prev", &cursor));
    let next = next.map(|cursor| cursor_link(base, query, "next", &cursor));

    Ok(view! {
        if total > 0 {
            <nav class="pager">
                <span class="muted">
                    "Showing " (shown_from) "–" (shown_to) " of " (total)
                </span>
                <span class="actions">
                    if let Some(href) = prev {
                        <a class="btn btn-sm" href=(href) rel="prev">"‹ Previous"</a>
                    } else {
                        <span class="btn btn-sm btn-disabled">"‹ Previous"</span>
                    }
                    if let Some(href) = next {
                        <a class="btn btn-sm" href=(href) rel="next">"Next ›"</a>
                    } else {
                        <span class="btn btn-sm btn-disabled">"Next ›"</span>
                    }
                </span>
            </nav>
        }
    })
}

/// Build a link that replaces the cursor parameter but keeps the filters.
fn cursor_link(base: &str, query: &str, key: &str, cursor: &str) -> String {
    let mut parts: Vec<String> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| pair.split('=').next() != Some(key))
        .map(str::to_string)
        .collect();
    parts.push(format!("{key}={}", auth::urlencode(cursor)));
    format!("{base}?{}", parts.join("&"))
}

/// Encode the filter parameters a list page is currently applying.
///
/// `None` and empty values are dropped, so the URL only carries what is set and
/// a cleared search box does not leave `?q=` behind.
#[must_use]
pub fn query_string(pairs: &[(&str, Option<String>)]) -> String {
    pairs
        .iter()
        .filter_map(|(name, value)| {
            let value = value.as_deref().filter(|value| !value.is_empty())?;
            Some(format!("{}={}", name, auth::urlencode(value)))
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// The configured display zone, read from the app context.
#[must_use]
pub fn zone(cx: &Cx) -> TimeZone {
    app_context::<TimeZone>(cx).clone()
}

/// Trim a form value and treat the empty string as absent.
///
/// Re-exported here so page modules import their form helpers from one place.
pub use domain::opt;
