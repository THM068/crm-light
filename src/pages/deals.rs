//! Deal pages: pipeline list, create, detail, stage moves, edit, delete.
//!
//! The list resolves the company and contact names for exactly the deals on
//! screen, in one query each, rather than reading both whole tables and joining
//! them in memory.

use std::collections::HashMap;

use serde::Deserialize;

use crate::access::{self, Tenant};
use crate::auth;
use crate::db;
use crate::domain::{self, Stage};
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

path_param!(deal_id: i64, error = bad_request("Deal id must be a number"));

/// Newest first. `created_at` is when the deal was entered, but the primary key
/// is the sort column because it is unique: two deals entered in the same
/// second would otherwise have no defined order between them, and a tie-break
/// on an ambiguous key is what makes a row appear on two pages or none.
const SORT: Sort = Sort::desc("id");

#[query_params(error = bad_request)]
struct ListQuery {
    q: Option<String>,
    stage: Option<String>,
    next: Option<String>,
    prev: Option<String>,
}

/// One row of the pipeline list.
struct Row {
    id: i64,
    title: String,
    stage: String,
    value_cents: i64,
    company: String,
    contact: String,
    close: String,
}

/// Load a deal for this workspace, or 403/404.
async fn find(db: &mut toasty::Db, tenant: Tenant, id: i64) -> Result<Deal> {
    access::require::<Deal>(db, tenant.account_id, id).await
}

/// `(id, label)` pairs for the company picker, scoped to the workspace.
async fn company_options(db: &mut toasty::Db, tenant: Tenant) -> Result<Vec<(i64, String)>> {
    Ok(Company::filter(Company::fields().account_id().eq(tenant.account_id))
        .order_by(Company::fields().name().asc())
        .exec(db)
        .await?
        .into_iter()
        .map(|company| (company.id, company.name))
        .collect())
}

/// `(id, label)` pairs for the contact picker, scoped to the workspace.
async fn contact_options(db: &mut toasty::Db, tenant: Tenant) -> Result<Vec<(i64, String)>> {
    Ok(Contact::filter(Contact::fields().account_id().eq(tenant.account_id))
        .order_by(Contact::fields().last_name().asc())
        .exec(db)
        .await?
        .into_iter()
        .map(|contact| {
            (
                contact.id,
                domain::full_name(&contact.first_name, &contact.last_name),
            )
        })
        .collect())
}

// --- List ------------------------------------------------------------------

#[page("/deals")]
async fn index(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let filters = query_params::<ListQuery>(cx)?;
    let search = filters.q.clone().unwrap_or_default().trim().to_string();
    let page_size = crate::config_of(cx).page_size;

    let stage_filter = filters
        .stage
        .as_deref()
        .and_then(Stage::parse)
        .unwrap_or(Stage::Lead);
    let stage_active = filters
        .stage
        .as_deref()
        .and_then(Stage::parse)
        .is_some();

    // One filter value for both the count and the page query, with the tenant
    // clause built in rather than added later.
    let mut filter = Deal::fields().account_id().eq(tenant.account_id);
    if !search.is_empty() {
        filter = filter.and(Deal::fields().title().contains_ignoring_case(&search));
    }
    if stage_active {
        filter = filter.and(Deal::fields().stage().eq(stage_filter.as_str()));
    }

    let total = Deal::all()
        .filter(filter.clone())
        .count()
        .exec(&mut db)
        .await? as usize;

    let pagination = Pagination {
        next: filters.next.clone(),
        prev: filters.prev.clone(),
    };
    let position = crate::pages::position(&pagination, SORT);
    let zone = views::zone(cx);

    let rows = Deal::all()
        .filter(filter)
        .order_by(Deal::fields().id().desc())
        .limit(pagination::fetch_limit(page_size))
        .offset(position.offset)
        .exec(&mut db)
        .await?;

    let page = pagination::assemble(rows, page_size, position.offset, total, SORT);
    let shown_from = page.showing_from();
    let shown_to = page.showing_to();

    let company_ids: Vec<i64> = page
        .rows
        .iter()
        .filter_map(|deal| deal.company_id)
        .collect();
    let contact_ids: Vec<i64> = page
        .rows
        .iter()
        .filter_map(|deal| deal.contact_id)
        .collect();

    let companies: HashMap<i64, String> = if company_ids.is_empty() {
        HashMap::new()
    } else {
        Company::filter(
            Company::fields()
                .account_id()
                .eq(tenant.account_id)
                .and(Company::fields().id().in_list(company_ids)),
        )
        .exec(&mut db)
        .await?
            .into_iter()
            .map(|company| (company.id, company.name))
            .collect()
    };
    let contacts: HashMap<i64, String> = if contact_ids.is_empty() {
        HashMap::new()
    } else {
        Contact::filter(
            Contact::fields()
                .account_id()
                .eq(tenant.account_id)
                .and(Contact::fields().id().in_list(contact_ids)),
        )
        .exec(&mut db)
        .await?
            .into_iter()
            .map(|contact| {
                (
                    contact.id,
                    domain::full_name(&contact.first_name, &contact.last_name),
                )
            })
            .collect()
    };

    let total_open: i64 = page
        .rows
        .iter()
        .filter(|deal| Stage::from_stored(&deal.stage).is_open())
        .map(|deal| deal.value_cents)
        .sum();

    let rows: Vec<Row> = page
        .rows
        .into_iter()
        .map(|deal| Row {
            company: deal
                .company_id
                .and_then(|id| companies.get(&id).cloned())
                .unwrap_or_default(),
            contact: deal
                .contact_id
                .and_then(|id| contacts.get(&id).cloned())
                .unwrap_or_default(),
            close: deal
                .expected_close
                .map(|t| zone.format_date(t))
                .unwrap_or_default(),
            id: deal.id,
            title: deal.title,
            stage: deal.stage,
            value_cents: deal.value_cents,
        })
        .collect();

    let stage_param = filters
        .stage
        .clone()
        .filter(|_| stage_active);
    let query = query_string(&[
        ("q", Some(search.clone())),
        ("stage", stage_param),
    ]);

    Ok(view! {
        <div class="page-head">
            <h1>"Deals"</h1>
            <a class="btn btn-primary" href="/deals/new">"New deal"</a>
        </div>

        <form class="searchbar" method="get" action="/deals">
            <input type="search" name="q" value=(search) placeholder="Search by title">
            <select name="stage" style="max-width: 12rem;">
                <option value="" selected=(!stage_active)>"All stages"</option>
                for stage in Stage::ALL {
                    <option value=(stage.as_str()) selected=(stage_active && stage == stage_filter)>
                        (stage.label())
                    </option>
                }
            </select>
            <button class="btn" type="submit">"Filter"</button>
        </form>

        if rows.is_empty() {
            <p class="empty">"No deals match."</p>
        } else {
            <table>
                <thead>
                    <tr>
                        <th>"Deal"</th>
                        <th>"Stage"</th>
                        <th class="num">"Value"</th>
                        <th>"Company"</th>
                        <th>"Contact"</th>
                        <th>"Expected close"</th>
                    </tr>
                </thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td><a href=(format!("/deals/{}", row.id))>(row.title)</a></td>
                            <td>stage_badge(stage: row.stage.as_str())</td>
                            <td class="num">(domain::format_money(row.value_cents))</td>
                            <td class="muted">(row.company)</td>
                            <td class="muted">(row.contact)</td>
                            <td class="muted">(row.close)</td>
                        </tr>
                    }
                </tbody>
            </table>
            <p class="muted">
                "Open pipeline on this page: "
                (domain::format_money(total_open))
            </p>
        }

        pager(
            base: "/deals",
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

#[page("/deals/new")]
async fn new_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    auth::require_user(cx)?;
    let companies = company_options(&mut db, tenant).await?;
    let contacts = contact_options(&mut db, tenant).await?;

    Ok(view! {
        <div class="page-head">
            <h1>"New deal"</h1>
            <a class="btn" href="/deals">"Cancel"</a>
        </div>
        <div class="panel">
            deal_form(
                action: "/deals".to_string(),
                title: String::new(),
                value: String::new(),
                stage: Stage::Lead.as_str().to_string(),
                expected_close: String::new(),
                notes: String::new(),
                companies: companies,
                contacts: contacts,
                selected_company: None,
                selected_contact: None,
                submit_label: "Create deal",
            )
        </div>
    })
}

#[derive(Deserialize)]
struct DealForm {
    title: String,
    #[serde(default)]
    value: String,
    #[serde(default)]
    stage: String,
    #[serde(default)]
    expected_close: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    company_id: String,
    #[serde(default)]
    contact_id: String,
}

/// Read a `company_id`/`contact_id` form field: empty means "none".
fn form_id(value: String) -> Option<i64> {
    domain::opt(value).and_then(|id| id.parse().ok())
}

/// Read a `value` form field: empty means zero.
fn form_money(value: String) -> i64 {
    domain::opt(value).and_then(|v| domain::parse_money(&v)).unwrap_or(0)
}

/// Check that the company and contact a form named belong to this workspace.
///
/// A deal pointing at another workspace's company would be a link between two
/// tenants, which is exactly what this refuses.
async fn checked_links(
    db: &mut toasty::Db,
    tenant: Tenant,
    company_id: Option<i64>,
    contact_id: Option<i64>,
) -> Result<(Option<i64>, Option<i64>)> {
    let company = access::require_reference::<Company>(db, tenant.account_id, company_id).await?;
    let contact = access::require_reference::<Contact>(db, tenant.account_id, contact_id).await?;
    Ok((company, contact))
}

#[route(POST "/deals")]
async fn create(cx: &Cx, body: crate::csrf::CsrfForm<DealForm>) -> Result<SeeOther> {
    let tenant = Tenant::of(cx)?;

    let config = crate::config_of(cx);
    let crate::csrf::CsrfForm(input) = body;
    let zone = views::zone(cx);
    let title = input.title.trim();
    if title.is_empty() {
        return Err(bad_request("Deal title is required").into());
    }

    // Both references must be in this workspace, checked before the row is
    // written rather than left dangling.
    let (company_id, contact_id) = checked_links(
        &mut db(cx),
        tenant,
        domain::opt(input.company_id.clone()).and_then(|id| id.parse().ok()),
        domain::opt(input.contact_id.clone()).and_then(|id| id.parse().ok()),
    )
    .await?;

    let deal = toasty::create!(Deal {
        account_id: tenant.account_id,
        title,
        value_cents: form_money(input.value),
        stage: Stage::parse(input.stage.trim()).unwrap_or(Stage::Lead).as_str(),
        company_id,
        contact_id,
        // Parsed in the display zone, so the date shown back is the date typed.
        expected_close: domain::opt(input.expected_close)
            .and_then(|d| domain::parse_date_in(&d, &zone)),
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
        &format!("Added {}.", deal.title),
    );
    Ok(see_other(format!("/deals/{}", deal.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/deals/{deal_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let id = *path_param::<DealId>(cx)?;
    let deal = find(&mut db, tenant, id).await?;
    let zone = views::zone(cx);

    let company = match deal.company_id {
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
    let contact = match deal.contact_id {
        Some(contact_id) => Contact::filter(
            Contact::fields()
                .account_id()
                .eq(tenant.account_id)
                .and(Contact::fields().id().eq(contact_id)),
        )
        .first()
        .exec(&mut db)
        .await?,
        None => None,
    };
    let panel = activity_panel(
        &mut db,
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().deal_id().eq(Some(id))),
        cx,
    )
    .await?;
    let (activities, authors) = (panel.activities, panel.authors);

    let company_name = company
        .as_ref()
        .map(|company| company.name.clone())
        .unwrap_or_default();
    let company_link = deal
        .company_id
        .map(|id| format!("/companies/{id}"))
        .unwrap_or_default();
    let contact_name = contact
        .as_ref()
        .map(|contact| domain::full_name(&contact.first_name, &contact.last_name))
        .unwrap_or_default();
    let contact_link = deal
        .contact_id
        .map(|id| format!("/contacts/{id}"))
        .unwrap_or_default();

    Ok(view! {
        <div class="page-head">
            <h1>(&deal.title)</h1>
            <div class="actions">
                <a class="btn" href=(format!("/deals/{id}/edit"))>"Edit"</a>
                <form class="inline" method="post" action=(format!("/deals/{id}/delete"))>
                    csrf_field()
                    <button class="btn btn-danger" type="submit">"Delete"</button>
                </form>
            </div>
        </div>

        <div class="cols">
            <div>
                <div class="panel">
                    <dl class="meta">
                        <dt>"Stage"</dt>
                        <dd>stage_badge(stage: deal.stage.as_str())</dd>
                        <dt>"Value"</dt>
                        <dd>(domain::format_money(deal.value_cents))</dd>
                        <dt>"Company"</dt>
                        <dd>
                            if company_name.is_empty() {
                                "—"
                            } else {
                                <a href=(company_link)>(company_name)</a>
                            }
                        </dd>
                        <dt>"Contact"</dt>
                        <dd>
                            if contact_name.is_empty() {
                                "—"
                            } else {
                                <a href=(contact_link)>(contact_name)</a>
                            }
                        </dd>
                        <dt>"Expected close"</dt>
                        <dd>
                            (deal.expected_close
                                .map(|t| zone.format_date(t))
                                .unwrap_or_else(|| "—".to_string()))
                        </dd>
                        <dt>"Notes"</dt>
                        <dd>(deal.notes.as_deref().unwrap_or("—"))</dd>
                        <dt>"Created"</dt>
                        <dd>(zone.format_date(deal.created_at))</dd>
                    </dl>
                </div>

                <h2>"Move stage"</h2>
                <div class="panel">
                    <form method="post" action=(format!("/deals/{id}/stage"))>
                        csrf_field()
                        <div class="form-row">
                            <div class="field">
                                <label for="stage">"Stage"</label>
                                <select id="stage" name="stage">
                                    for stage in Stage::ALL {
                                        <option value=(stage.as_str()) selected=(stage.as_str() == deal.stage)>
                                            (stage.label())
                                        </option>
                                    }
                                </select>
                            </div>
                        </div>
                        <button class="btn btn-primary" type="submit">"Update stage"</button>
                    </form>
                </div>
            </div>

            <div>
                <h2>"Log activity"</h2>
                <div class="panel">
                    activity_form(
                        action: "/activities",
                        company_id: deal.company_id,
                        contact_id: deal.contact_id,
                        deal_id: Some(id),
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

// --- Stage -----------------------------------------------------------------

#[derive(Deserialize)]
struct StageForm {
    stage: String,
}

#[route(POST "/deals/{deal_id}/stage")]
async fn set_stage(cx: &Cx, body: crate::csrf::CsrfForm<StageForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(input) = body;
    auth::require_user(cx)?;
    let id = *path_param::<DealId>(cx)?;
    let stage = Stage::parse(input.stage.trim()).ok_or_else(|| bad_request("Unknown stage"))?;

    let mut deal = find(&mut db, tenant, id).await?;
    toasty::update!(deal { stage: stage.as_str() })
        .exec(&mut db)
        .await?;

    Ok(see_other(format!("/deals/{id}")))
}

// --- Edit ------------------------------------------------------------------

#[page("/deals/{deal_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    auth::require_user(cx)?;
    let id = *path_param::<DealId>(cx)?;
    let deal = find(&mut db, tenant, id).await?;
    let companies = company_options(&mut db, tenant).await?;
    let contacts = contact_options(&mut db, tenant).await?;
    let zone = views::zone(cx);

    Ok(view! {
        <div class="page-head">
            <h1>"Edit deal"</h1>
            <a class="btn" href=(format!("/deals/{id}"))>"Cancel"</a>
        </div>
        <div class="panel">
            deal_form(
                action: format!("/deals/{id}"),
                title: deal.title.clone(),
                value: domain::money_input_value(deal.value_cents),
                stage: deal.stage.clone(),
                expected_close: deal.expected_close.map(|t| zone.format_date(t)).unwrap_or_default(),
                notes: deal.notes.clone().unwrap_or_default(),
                companies: companies,
                contacts: contacts,
                selected_company: deal.company_id,
                selected_contact: deal.contact_id,
                submit_label: "Save changes",
            )
        </div>
    })
}

#[route(POST "/deals/{deal_id}")]
async fn update(cx: &Cx, body: crate::csrf::CsrfForm<DealForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    let crate::csrf::CsrfForm(input) = body;
    auth::require_user(cx)?;
    let id = *path_param::<DealId>(cx)?;
    let zone = views::zone(cx);

    let title = input.title.trim();
    if title.is_empty() {
        return Err(bad_request("Deal title is required").into());
    }

    let (company_id, contact_id) = checked_links(
        &mut db,
        tenant,
        form_id(input.company_id.clone()),
        form_id(input.contact_id.clone()),
    )
    .await?;

    let mut deal = find(&mut db, tenant, id).await?;
    toasty::update!(deal {
        title,
        value_cents: form_money(input.value),
        stage: Stage::parse(input.stage.trim()).unwrap_or(Stage::Lead).as_str(),
        company_id,
        contact_id,
        expected_close: domain::opt(input.expected_close)
            .and_then(|d| domain::parse_date_in(&d, &zone)),
        notes: domain::opt(input.notes),
    })
    .exec(&mut db)
    .await?;

    Ok(see_other(format!("/deals/{id}")))
}

// --- Delete ----------------------------------------------------------------

#[route(POST "/deals/{deal_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = db(cx);
    let tenant = Tenant::of(cx)?;

    auth::require_user(cx)?;
    let id = *path_param::<DealId>(cx)?;

    find(&mut db, tenant, id).await?;

    for mut activity in Activity::filter(
        Activity::fields()
            .account_id()
            .eq(tenant.account_id)
            .and(Activity::fields().deal_id().eq(Some(id))),
    )
    .exec(&mut db)
    .await?
    {
        toasty::update!(activity { deal_id: Option::<i64>::None })
            .exec(&mut db)
            .await?;
    }

    Deal::delete_by_id(&mut db, id).await?;

    Ok(see_other("/deals"))
}

// --- Shared form -----------------------------------------------------------

#[component]
async fn entity_select(
    id: &str,
    options: Vec<(i64, String)>,
    selected: Option<i64>,
) -> Result<impl View> {
    Ok(view! {
        <select id=(id) name=(id)>
            <option value="">"— none —"</option>
            for (value, label) in &options {
                <option value=(value) selected=(Some(*value) == selected)>(label)</option>
            }
        </select>
    })
}

/// Create/edit form, shared by both pages.
#[component]
async fn deal_form(
    action: String,
    title: String,
    value: String,
    stage: String,
    expected_close: String,
    notes: String,
    companies: Vec<(i64, String)>,
    contacts: Vec<(i64, String)>,
    selected_company: Option<i64>,
    selected_contact: Option<i64>,
    submit_label: &str,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
            csrf_field()
            <div class="field">
                <label for="title">"Title"</label>
                <input id="title" name="title" value=(title) required="" autofocus="">
            </div>
            <div class="form-row">
                <div class="field">
                    <label for="value">"Value"</label>
                    <input id="value" name="value" value=(value) placeholder="0.00" inputmode="decimal">
                </div>
                <div class="field">
                    <label for="stage">"Stage"</label>
                    <select id="stage" name="stage">
                        for option in Stage::ALL {
                            <option value=(option.as_str()) selected=(option.as_str() == stage)>
                                (option.label())
                            </option>
                        }
                    </select>
                </div>
                <div class="field">
                    <label for="expected_close">"Expected close"</label>
                    <input id="expected_close" name="expected_close" type="date" value=(expected_close)>
                </div>
            </div>
            <div class="form-row">
                <div class="field">
                    <label for="company_id">"Company"</label>
                    entity_select(id: "company_id", options: companies, selected: selected_company)
                </div>
                <div class="field">
                    <label for="contact_id">"Contact"</label>
                    entity_select(id: "contact_id", options: contacts, selected: selected_contact)
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
