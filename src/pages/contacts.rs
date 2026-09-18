//! Contact pages: list, create, detail, edit, delete.

use std::collections::HashMap;

use serde::Deserialize;

use crate::access::{self, Tenant};
use crate::auth;
use crate::db;
use crate::domain;
use crate::flash;
use crate::models::{Activity, Company, Contact, Deal};
use crate::pages::{Pagination, activity_panel, creator};
use crate::pagination::{self, Sort};
use crate::search::CaseInsensitiveLike;
use crate::views::{self, activity_feed, activity_form, csrf_field, pager, query_string, stage_badge};
use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{SeeOther, bad_request, see_other},
        page, path_param, query_params, route,
    },
    view::{View, component, view},
};

path_param!(contact_id: i64, error = bad_request("Contact id must be a number"));

/// The column the list is ordered by.
const SORT: Sort = Sort::asc("last_name");

#[query_params(error = bad_request)]
struct ListQuery {
    q: Option<String>,
    next: Option<String>,
    prev: Option<String>,
}

#[query_params(error = bad_request)]
struct NewContact {
    company_id: Option<i64>,
}

/// One row of the contact list.
struct Row {
    id: i64,
    name: String,
    title: String,
    email: String,
    phone: String,
    company: String,
}

/// Load a contact for this workspace, or 403/404.
async fn find(db: &mut toasty::Db, tenant: Tenant, id: i64) -> Result<Contact> {
    access::require::<Contact>(db, tenant.account_id, id).await
}

/// `(id, name)` for every company in the workspace, for the company picker.
///
/// Scoped, so one workspace's contacts can never be attached to another's
/// company by picking it from a list.
async fn company_options(db: &mut toasty::Db, tenant: Tenant) -> Result<Vec<(i64, String)>> {
    Ok(Company::filter(Company::fields().account_id().eq(tenant.account_id))
        .order_by(Company::fields().name().asc())
        .exec(db)
        .await?
        .into_iter()
        .map(|company| (company.id, company.name))
        .collect())
}

/// Resolve the company names for exactly the contacts on screen.
async fn company_names(
    db: &mut toasty::Db,
    tenant: Tenant,
    ids: &[i64],
) -> Result<HashMap<i64, String>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let companies = Company::filter(
        Company::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Company::fields().id().in_list(ids.to_vec())),
    )
    .exec(db)
    .await?;
    Ok(companies
        .into_iter()
        .map(|company| (company.id, company.name))
        .collect())
}

// --- List ------------------------------------------------------------------

#[page("/contacts")]
async fn index(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let params = query_params::<ListQuery>(cx)?;
    let search = params.q.clone().unwrap_or_default().trim().to_string();
    let page_size = crate::config_of(cx).page_size;

    // One filter value, used by both the count and the page query, with the
    // tenant clause built in rather than added later.
    let mut filter = Contact::fields().account_id().eq(tenant.account_id);
    if !search.is_empty() {
        filter = filter.and(
            Contact::fields()
                .first_name()
                .contains_ignoring_case(&search)
                .or(Contact::fields().last_name().contains_ignoring_case(&search))
                .or(Contact::fields().email().contains_ignoring_case(&search))
                .or(Contact::fields().title().contains_ignoring_case(&search)),
        );
    }

    let total = Contact::all()
        .filter(filter.clone())
        .count()
        .exec(&mut db)
        .await? as usize;

    let pagination = Pagination {
        next: params.next.clone(),
        prev: params.prev.clone(),
    };
    let position = crate::pages::position(&pagination, SORT);

    let rows = Contact::all()
        .filter(filter)
        .order_by(Contact::fields().last_name().asc())
        .limit(pagination::fetch_limit(page_size))
        .offset(position.offset)
        .exec(&mut db)
        .await?;

    let page = pagination::assemble(rows, page_size, position.offset, total, SORT);
    let shown_from = page.showing_from();
    let shown_to = page.showing_to();

    // Only the companies this page actually references.
    let company_ids: Vec<i64> = page
        .rows
        .iter()
        .filter_map(|contact| contact.company_id)
        .collect();
    let names = company_names(&mut db, tenant, &company_ids).await?;

    let rows: Vec<Row> = page
        .rows
        .into_iter()
        .map(|contact| Row {
            company: contact
                .company_id
                .and_then(|id| names.get(&id).cloned())
                .unwrap_or_default(),
            id: contact.id,
            name: domain::full_name(&contact.first_name, &contact.last_name),
            title: contact.title.unwrap_or_default(),
            email: contact.email.unwrap_or_default(),
            phone: contact.phone.unwrap_or_default(),
        })
        .collect();

    let query = query_string(&[("q", Some(search.clone()))]);

    Ok(view! {
        <div class="page-head">
            <h1>"Contacts"</h1>
            <a class="btn btn-primary" href="/contacts/new">"New contact"</a>
        </div>

        <form class="searchbar" method="get" action="/contacts">
            <input type="search" name="q" value=(search) placeholder="Search name, email, or title">
            <button class="btn" type="submit">"Search"</button>
        </form>

        if rows.is_empty() {
            <p class="empty">"No contacts match."</p>
        } else {
            <table>
                <thead>
                    <tr>
                        <th>"Name"</th>
                        <th>"Title"</th>
                        <th>"Company"</th>
                        <th>"Email"</th>
                        <th>"Phone"</th>
                    </tr>
                </thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td><a href=(format!("/contacts/{}", row.id))>(row.name)</a></td>
                            <td class="muted">(row.title)</td>
                            <td class="muted">(row.company)</td>
                            <td class="muted">(row.email)</td>
                            <td class="muted">(row.phone)</td>
                        </tr>
                    }
                </tbody>
            </table>
        }

        pager(
            base: "/contacts",
            query: &query,
            total: page.total,
            shown_from: shown_from,
            shown_to: shown_to,
            prev: page.prev.clone(),
            next: page.next.clone(),
        )
    })
}

// --- Create ----------------------------------------------------------------

#[page("/contacts/new")]
async fn new_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    auth::require_user(cx)?;
    let tenant = Tenant::of(cx)?;
    let preselected = query_params::<NewContact>(cx)?.company_id;
    // A preselected company from the query string is checked like any other
    // reference, so `?company_id=` cannot point at another workspace.
    let preselected = access::require_reference::<Company>(&mut db, tenant.account_id, preselected)
        .await?;
    let options = company_options(&mut db, tenant).await?;

    Ok(view! {
        <div class="page-head">
            <h1>"New contact"</h1>
            <a class="btn" href="/contacts">"Cancel"</a>
        </div>
        <div class="panel">
            contact_form(
                action: "/contacts".to_string(),
                first_name: String::new(),
                last_name: String::new(),
                email: String::new(),
                phone: String::new(),
                title: String::new(),
                notes: String::new(),
                companies: options,
                selected_company: preselected,
                submit_label: "Create contact",
            )
        </div>
    })
}

#[derive(Deserialize)]
struct ContactForm {
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    last_name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    notes: String,
    /// Empty string means "no company".
    #[serde(default)]
    company_id: String,
}

#[route(POST "/contacts")]
async fn create(cx: &Cx, body: crate::csrf::CsrfForm<ContactForm>) -> Result<SeeOther> {
    let tenant = Tenant::of(cx)?;

    let config = crate::config_of(cx);
    let crate::csrf::CsrfForm(input) = body;
    let first_name = input.first_name.trim();
    let last_name = input.last_name.trim();
    if first_name.is_empty() && last_name.is_empty() {
        return Err(bad_request("A first or last name is required").into());
    }

    // The company named by the form must be in this workspace, or the contact
    // would hang off somebody else's record.
    let company_id = access::require_reference::<Company>(
        &mut db(cx),
        tenant.account_id,
        domain::opt(input.company_id).and_then(|id| id.parse().ok()),
    )
    .await?;

    let contact = toasty::create!(Contact {
        account_id: tenant.account_id,
        first_name,
        last_name,
        email: domain::opt(input.email),
        phone: domain::opt(input.phone),
        title: domain::opt(input.title),
        company_id,
        notes: domain::opt(input.notes),
        created_at: domain::now(),
        created_by: creator(cx),
    })
    .exec(&mut db(cx))
    .await?;

    flash::set(
        cx,
        config,
        flash::Kind::Ok,
        &format!(
            "Added {}.",
            domain::full_name(&contact.first_name, &contact.last_name)
        ),
    );
    Ok(see_other(format!("/contacts/{}", contact.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/contacts/{contact_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let id = *path_param::<ContactId>(cx)?;
    let contact = find(&mut db, tenant, id).await?;
    let zone = views::zone(cx);

    let company = match contact.company_id {
        // The contact's own company is in the same workspace by construction;
        // the filter is here so a row that predates that rule cannot leak one.
        Some(company_id) => Company::filter(
            Company::fields()
                .account_id()
                .eq(tenant.account_id)
                .and(Company::fields().id().eq(company_id)),
        )
        .first()
        .exec(&mut db)
        .await?,
        None => None,
    };
    // Resolved here so the template only needs a plain `if`/`else`.
    let company_name = company
        .as_ref()
        .map(|company| company.name.clone())
        .unwrap_or_default();
    let company_link = contact
        .company_id
        .map(|id| format!("/companies/{id}"))
        .unwrap_or_default();
    let deals = Deal::filter(
        Deal::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Deal::fields().contact_id().eq(id)),
    )
    .order_by(Deal::fields().created_at().desc())
    .exec(&mut db)
    .await?;
    let panel = activity_panel(
        &mut db,
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().contact_id().eq(Some(id))),
        cx,
    )
    .await?;
    let (activities, authors) = (panel.activities, panel.authors);

    Ok(view! {
        <div class="page-head">
            <h1>(domain::full_name(&contact.first_name, &contact.last_name))</h1>
            <div class="actions">
                <a class="btn" href=(format!("/contacts/{id}/edit"))>"Edit"</a>
                <form class="inline" method="post" action=(format!("/contacts/{id}/delete"))>
                    csrf_field()
                    <button class="btn btn-danger" type="submit">"Delete"</button>
                </form>
            </div>
        </div>

        <div class="cols">
            <div>
                <div class="panel">
                    <dl class="meta">
                        <dt>"Company"</dt>
                        <dd>
                            if company_name.is_empty() {
                                "—"
                            } else {
                                <a href=(company_link)>(company_name)</a>
                            }
                        </dd>
                        <dt>"Title"</dt>
                        <dd>(contact.title.as_deref().unwrap_or("—"))</dd>
                        <dt>"Email"</dt>
                        <dd>(contact.email.as_deref().unwrap_or("—"))</dd>
                        <dt>"Phone"</dt>
                        <dd>(contact.phone.as_deref().unwrap_or("—"))</dd>
                        <dt>"Notes"</dt>
                        <dd>(contact.notes.as_deref().unwrap_or("—"))</dd>
                        <dt>"Added"</dt>
                        <dd>(zone.format_date(contact.created_at))</dd>
                    </dl>
                </div>

                <h2>"Deals"</h2>
                if deals.is_empty() {
                    <p class="empty">"No deals for this contact yet."</p>
                } else {
                    <table>
                        <thead>
                            <tr><th>"Deal"</th><th>"Stage"</th><th class="num">"Value"</th></tr>
                        </thead>
                        <tbody>
                            for deal in &deals {
                                <tr>
                                    <td><a href=(format!("/deals/{}", deal.id))>(&deal.title)</a></td>
                                    <td>stage_badge(stage: deal.stage.as_str())</td>
                                    <td class="num">(domain::format_money(deal.value_cents))</td>
                                </tr>
                            }
                        </tbody>
                    </table>
                }
            </div>

            <div>
                <h2>"Log activity"</h2>
                <div class="panel">
                    activity_form(
                        action: "/activities",
                        company_id: contact.company_id,
                        contact_id: Some(id),
                        deal_id: None,
                    )
                </div>

                <h2>"History"</h2>
                <div class="panel">
                    activity_feed(activities: activities, authors: authors, zone: zone)
                </div>
            </div>
        </div>
    })
}

// --- Edit ------------------------------------------------------------------

#[page("/contacts/{contact_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    auth::require_user(cx)?;
    let id = *path_param::<ContactId>(cx)?;
    let contact = find(&mut db, tenant, id).await?;
    let options = company_options(&mut db, tenant).await?;

    Ok(view! {
        <div class="page-head">
            <h1>"Edit contact"</h1>
            <a class="btn" href=(format!("/contacts/{id}"))>"Cancel"</a>
        </div>
        <div class="panel">
            contact_form(
                action: format!("/contacts/{id}"),
                first_name: contact.first_name.clone(),
                last_name: contact.last_name.clone(),
                email: contact.email.clone().unwrap_or_default(),
                phone: contact.phone.clone().unwrap_or_default(),
                title: contact.title.clone().unwrap_or_default(),
                notes: contact.notes.clone().unwrap_or_default(),
                companies: options,
                selected_company: contact.company_id,
                submit_label: "Save changes",
            )
        </div>
    })
}

#[route(POST "/contacts/{contact_id}")]
async fn update(cx: &Cx, body: crate::csrf::CsrfForm<ContactForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(input) = body;
    auth::require_user(cx)?;
    let id = *path_param::<ContactId>(cx)?;

    let first_name = input.first_name.trim();
    let last_name = input.last_name.trim();
    if first_name.is_empty() && last_name.is_empty() {
        return Err(bad_request("A first or last name is required").into());
    }

    let company_id = access::require_reference::<Company>(
        &mut db,
        tenant.account_id,
        domain::opt(input.company_id).and_then(|id| id.parse().ok()),
    )
    .await?;

    let mut contact = find(&mut db, tenant, id).await?;
    toasty::update!(contact {
        first_name,
        last_name,
        email: domain::opt(input.email),
        phone: domain::opt(input.phone),
        title: domain::opt(input.title),
        company_id,
        notes: domain::opt(input.notes),
    })
    .exec(&mut db)
    .await?;

    Ok(see_other(format!("/contacts/{id}")))
}

// --- Delete ----------------------------------------------------------------

#[route(POST "/contacts/{contact_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    auth::require_user(cx)?;
    let id = *path_param::<ContactId>(cx)?;

    find(&mut db, tenant, id).await?;

    for mut deal in Deal::filter(
        Deal::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Deal::fields().contact_id().eq(id)),
    )
    .exec(&mut db)
    .await?
    {
        toasty::update!(deal { contact_id: Option::<i64>::None })
            .exec(&mut db)
            .await?;
    }
    for mut activity in Activity::filter(
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().contact_id().eq(Some(id))),
    )
    .exec(&mut db)
    .await?
    {
        toasty::update!(activity { contact_id: Option::<i64>::None })
            .exec(&mut db)
            .await?;
    }

    Contact::delete_by_id(&mut db, id).await?;

    Ok(see_other("/contacts"))
}

// --- Shared form -----------------------------------------------------------

#[component]
async fn company_select(companies: Vec<(i64, String)>, selected: Option<i64>) -> Result<impl View> {
    Ok(view! {
        <select id="company_id" name="company_id">
            <option value="">"— none —"</option>
            for (company_id, name) in &companies {
                <option value=(company_id) selected=(Some(*company_id) == selected)>(name)</option>
            }
        </select>
    })
}

/// Create/edit form, shared by both pages.
#[component]
async fn contact_form(
    action: String,
    first_name: String,
    last_name: String,
    email: String,
    phone: String,
    title: String,
    notes: String,
    companies: Vec<(i64, String)>,
    selected_company: Option<i64>,
    submit_label: &str,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
            csrf_field()
            <div class="form-row">
                <div class="field">
                    <label for="first_name">"First name"</label>
                    <input id="first_name" name="first_name" value=(first_name) autofocus="">
                </div>
                <div class="field">
                    <label for="last_name">"Last name"</label>
                    <input id="last_name" name="last_name" value=(last_name)>
                </div>
            </div>
            <div class="form-row">
                <div class="field">
                    <label for="email">"Email"</label>
                    <input id="email" name="email" type="email" value=(email)>
                </div>
                <div class="field">
                    <label for="phone">"Phone"</label>
                    <input id="phone" name="phone" value=(phone)>
                </div>
            </div>
            <div class="form-row">
                <div class="field">
                    <label for="title">"Job title"</label>
                    <input id="title" name="title" value=(title)>
                </div>
                <div class="field">
                    <label for="company_id">"Company"</label>
                    company_select(companies: companies, selected: selected_company)
                </div>
            </div>
            <div class="field">
                <label for="notes">"Notes"</label>
                <textarea id="notes" name="notes">(notes)</textarea>
            </div>
            <button class="btn btn-primary" type="submit">(submit_label)</button>
        </form>
    })
}
