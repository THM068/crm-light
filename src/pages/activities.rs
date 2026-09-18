//! Activity logging: create and delete.
//!
//! There is no standalone activity page — the form is embedded on company,
//! contact, and deal pages, and posting returns to whichever record it was
//! logged against.

use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{SeeOther, bad_request, see_other},
        path_param, route,
    },
};

use crate::access::{self, Tenant};
use crate::auth;
use crate::db;
use crate::domain::{self, ActivityKind};
use crate::models::{Activity, Company, Contact, Deal};
use crate::pages::creator;

path_param!(activity_id: i64, error = bad_request("Activity id must be a number"));

#[derive(Deserialize)]
struct ActivityForm {
    #[serde(default)]
    kind: String,
    body: String,
    /// The three association fields are always submitted; unused ones are empty.
    #[serde(default)]
    company_id: String,
    #[serde(default)]
    contact_id: String,
    #[serde(default)]
    deal_id: String,
}

/// Parse a hidden id field: empty means "not associated".
fn form_id(value: String) -> Option<i64> {
    domain::opt(value).and_then(|id| id.parse().ok())
}

#[route(POST "/activities")]
async fn create(cx: &Cx, body: crate::csrf::CsrfForm<ActivityForm>) -> Result<SeeOther> {
    let tenant = Tenant::of(cx)?;

    auth::require_user(cx)?;
    let crate::csrf::CsrfForm(input) = body;

    let body = input.body.trim();
    if body.is_empty() {
        return Err(bad_request("Activity note is required").into());
    }

    // Every association the form named must be in this workspace: an activity
    // is a note *about* a record, and pointing one at another tenant's company
    // would both leak that the row exists and attach a note nobody else can
    // see.
    let company_id = access::require_reference::<Company>(
        &mut db(cx),
        tenant.account_id,
        form_id(input.company_id),
    )
    .await?;
    let contact_id = access::require_reference::<Contact>(
        &mut db(cx),
        tenant.account_id,
        form_id(input.contact_id),
    )
    .await?;
    let deal_id =
        access::require_reference::<Deal>(&mut db(cx), tenant.account_id, form_id(input.deal_id))
            .await?;

    toasty::create!(Activity {
        account_id: tenant.account_id,
        kind: ActivityKind::from_stored(input.kind.trim()).as_str(),
        body,
        company_id,
        contact_id,
        deal_id,
        created_at: domain::now(),
        // The audit trail: who logged this, not only when.
        user_id: creator(cx),
    })
    .exec(&mut db(cx))
    .await?;

    // Return to the record the note was logged against, most specific first.
    let back = if let Some(id) = deal_id {
        format!("/deals/{id}")
    } else if let Some(id) = contact_id {
        format!("/contacts/{id}")
    } else if let Some(id) = company_id {
        format!("/companies/{id}")
    } else {
        "/".to_string()
    };

    Ok(see_other(back))
}

#[route(POST "/activities/{activity_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    auth::require_user(cx)?;
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let id = *path_param::<ActivityId>(cx)?;

    // Refused before the delete, so a cross-tenant id cannot remove a row.
    let activity = access::require::<Activity>(&mut db, tenant.account_id, id).await?;
    let activity = Some(activity);

    let back = match &activity {
        Some(activity) if activity.deal_id.is_some() => {
            format!("/deals/{}", activity.deal_id.unwrap_or_default())
        }
        Some(activity) if activity.contact_id.is_some() => {
            format!("/contacts/{}", activity.contact_id.unwrap_or_default())
        }
        Some(activity) if activity.company_id.is_some() => {
            format!("/companies/{}", activity.company_id.unwrap_or_default())
        }
        _ => "/".to_string(),
    };

    Activity::delete_by_id(&mut db, id).await?;

    Ok(see_other(back))
}
