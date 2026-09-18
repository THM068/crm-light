//! Page handlers and shared view components.

pub mod activities;
pub mod companies;
pub mod contacts;
pub mod dashboard;
pub mod deals;

use crate::domain::{self, ActivityKind, Stage};
use crate::models::Activity;
use topcoat::{
    Result,
    view::{View, component, view},
};

/// Render an optional id for a hidden form input; absent becomes empty.
fn id_value(id: Option<u64>) -> String {
    id.map(|id| id.to_string()).unwrap_or_default()
}

/// Coloured badge showing a deal's stage.
#[component]
pub async fn stage_badge(stage: &str) -> Result<impl View> {
    let stage = Stage::from_stored(stage);
    Ok(view! {
        <span class=(format!("badge {}", stage.css_class()))>(stage.label())</span>
    })
}

/// A chronological list of logged activities, newest first.
#[component]
pub async fn activity_feed(activities: Vec<Activity>) -> Result<impl View> {
    Ok(view! {
        if activities.is_empty() {
            <p class="empty">"Nothing logged yet."</p>
        } else {
            for activity in activities {
                <div class="activity">
                    <div class="head">
                        <strong>(ActivityKind::from_stored(&activity.kind).label())</strong>
                        <span class="actions">
                            <span>(domain::format_datetime(activity.created_at))</span>
                            <form class="inline" method="post" action=(format!("/activities/{}/delete", activity.id))>
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

/// Inline form for logging an activity against a company, contact, or deal.
///
/// All three ids are submitted; the ones that do not apply are sent empty and
/// read back as `None`.
#[component]
pub async fn activity_form(
    action: &str,
    company_id: Option<u64>,
    contact_id: Option<u64>,
    deal_id: Option<u64>,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
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
