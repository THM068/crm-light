//! Company pages: list, create, detail, edit, delete.
//!
//! # Rollups
//!
//! The list shows a contact count and an open-pipeline total per company. Both
//! are computed with one query for the whole page, filtered to the ids actually
//! on screen, rather than by reading every contact and every deal in the
//! database and grouping them in memory. That was the part of this page whose
//! cost grew with the size of the database rather than the size of the page.

use std::collections::HashMap;

use serde::Deserialize;

use crate::auth;
use crate::db;
use crate::domain::{self, Stage};
use crate::flash;
use crate::models::{Activity, Company, Contact, Deal};
use crate::pages::{ActivityPanel, Pagination, activity_panel, creator};
use crate::pagination::{self, Sort};
use crate::search::CaseInsensitiveLike;
use crate::views::{self, activity_feed, activity_form, csrf_field, pager, query_string, stage_badge};
use topcoat::{
    Result,
    context::Cx,
    router::{
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        page, path_param, query_params, route,
    },
    view::{View, component, view},
};

path_param!(company_id: i64, error = bad_request("Company id must be a number"));

/// The column the list is ordered by.
const SORT: Sort = Sort::asc("name");

#[query_params(error = bad_request)]
struct ListQuery {
    q: Option<String>,
    next: Option<String>,
    prev: Option<String>,
}

/// One row of the company list.
struct Row {
    id: i64,
    name: String,
    industry: String,
    contacts: usize,
    open_value_cents: i64,
}

/// Load a company or answer 404.
async fn find(db: &mut toasty::Db, id: i64) -> Result<Company> {
    Company::filter(Company::fields().id().eq(id))
        .first()
        .exec(db)
        .await?
        .ok_or_not_found()
        .map_err(Into::into)
}

/// Contact counts and open pipeline for exactly the companies on screen.
///
/// Two queries in total, not two per row: one for the counts, one for the open
/// deals, both scoped with `IN (…)` to the page's ids.
async fn rollups(
    db: &mut toasty::Db,
    ids: &[i64],
) -> Result<(HashMap<i64, usize>, HashMap<i64, i64>)> {
    if ids.is_empty() {
        return Ok((HashMap::new(), HashMap::new()));
    }

    let contacts = Contact::filter(Contact::fields().company_id().in_list(ids.to_vec()))
        .exec(db)
        .await?;
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for contact in contacts {
        if let Some(company_id) = contact.company_id {
            *counts.entry(company_id).or_default() += 1;
        }
    }

    // Only open stages contribute to the pipeline figure, and the stage lives
    // in the row, so the filter is on the status list rather than on a value.
    let open_stages: Vec<String> = Stage::ALL
        .into_iter()
        .filter(|stage| stage.is_open())
        .map(|stage| stage.as_str().to_string())
        .collect();
    let deals = Deal::filter(
        Deal::fields()
            .company_id()
            .in_list(ids.to_vec())
            .and(Deal::fields().stage().in_list(open_stages)),
    )
    .exec(db)
    .await?;

    let mut open_value: HashMap<i64, i64> = HashMap::new();
    for deal in deals {
        if let Some(company_id) = deal.company_id {
            *open_value.entry(company_id).or_default() += deal.value_cents;
        }
    }

    Ok((counts, open_value))
}

// --- List ------------------------------------------------------------------

#[page("/companies")]
async fn index(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let params = query_params::<ListQuery>(cx)?;
    let search = params.q.clone().unwrap_or_default().trim().to_string();
    let page_size = crate::config_of(cx).page_size;

    // The filter is built once and used by both the count and the page query,
    // so the two can never describe different result sets.
    let filter = (!search.is_empty())
        .then(|| Company::fields().name().contains_ignoring_case(&search));

    let total = match &filter {
        Some(filter) => Company::all().filter(filter.clone()).count().exec(&mut db).await?,
        None => Company::all().count().exec(&mut db).await?,
    } as usize;

    let pagination = Pagination {
        next: params.next.clone(),
        prev: params.prev.clone(),
    };
    let position = crate::pages::position(&pagination, SORT);

    let mut query = Company::all().order_by(Company::fields().name().asc());
    if let Some(filter) = filter {
        query = query.filter(filter);
    }
    let rows = query
        .limit(pagination::fetch_limit(page_size))
        .offset(position.offset)
        .exec(&mut db)
        .await?;

    let page = pagination::assemble(rows, page_size, position.offset, total, SORT);
    let shown_from = page.showing_from();
    let shown_to = page.showing_to();

    let ids: Vec<i64> = page.rows.iter().map(|company| company.id).collect();
    let (contact_counts, open_values) = rollups(&mut db, &ids).await?;

    let rows: Vec<Row> = page
        .rows
        .into_iter()
        .map(|company| Row {
            contacts: contact_counts.get(&company.id).copied().unwrap_or(0),
            open_value_cents: open_values.get(&company.id).copied().unwrap_or(0),
            id: company.id,
            name: company.name,
            industry: company.industry.unwrap_or_default(),
        })
        .collect();

    let query = query_string(&[("q", Some(search.clone()))]);

    Ok(view! {
        <div class="page-head">
            <h1>"Companies"</h1>
            <a class="btn btn-primary" href="/companies/new">"New company"</a>
        </div>

        <form class="searchbar" method="get" action="/companies">
            <input type="search" name="q" value=(search) placeholder="Search by name">
            <button class="btn" type="submit">"Search"</button>
        </form>

        if rows.is_empty() {
            <p class="empty">"No companies match."</p>
        } else {
            <table>
                <thead>
                    <tr>
                        <th>"Company"</th>
                        <th>"Industry"</th>
                        <th class="num">"Contacts"</th>
                        <th class="num">"Open pipeline"</th>
                    </tr>
                </thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td><a href=(format!("/companies/{}", row.id))>(row.name)</a></td>
                            <td class="muted">(row.industry)</td>
                            <td class="num">(row.contacts)</td>
                            <td class="num">(domain::format_money(row.open_value_cents))</td>
                        </tr>
                    }
                </tbody>
            </table>
        }

        pager(
            base: "/companies",
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

#[page("/companies/new")]
async fn new_form(cx: &Cx) -> Result<impl View> {
    auth::require_user(cx)?;

    Ok(view! {
        <div class="page-head">
            <h1>"New company"</h1>
            <a class="btn" href="/companies">"Cancel"</a>
        </div>
        <div class="panel">
            company_form(
                action: "/companies".to_string(),
                name: String::new(),
                industry: String::new(),
                website: String::new(),
                phone: String::new(),
                notes: String::new(),
                submit_label: "Create company",
            )
        </div>
    })
}

#[derive(Deserialize)]
struct CompanyForm {
    name: String,
    #[serde(default)]
    industry: String,
    #[serde(default)]
    website: String,
    #[serde(default)]
    phone: String,
    #[serde(default)]
    notes: String,
}

#[route(POST "/companies")]
async fn create(cx: &Cx, body: crate::csrf::CsrfForm<CompanyForm>) -> Result<SeeOther> {
    let config = crate::config_of(cx);
    let crate::csrf::CsrfForm(input) = body;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(bad_request("Company name is required").into());
    }

    let company = toasty::create!(Company {
        name,
        industry: domain::opt(input.industry),
        website: domain::opt(input.website),
        phone: domain::opt(input.phone),
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
        &format!("Added {}.", company.name),
    );
    Ok(see_other(format!("/companies/{}", company.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/companies/{company_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let id = *path_param::<CompanyId>(cx)?;
    let company = find(&mut db, id).await?;
    let zone = views::zone(cx);

    let contacts = Contact::filter(Contact::fields().company_id().eq(id))
        .order_by(Contact::fields().last_name().asc())
        .exec(&mut db)
        .await?;
    let deals = Deal::filter(Deal::fields().company_id().eq(id))
        .order_by(Deal::fields().created_at().desc())
        .exec(&mut db)
        .await?;
    let panel = activity_panel(
        &mut db,
        Activity::fields().company_id().eq(Some(id)),
        cx,
    )
    .await?;
    let ActivityPanel {
        activities,
        authors,
        zone: _,
    } = panel;

    let open_value: i64 = deals
        .iter()
        .filter(|deal| Stage::from_stored(&deal.stage).is_open())
        .map(|deal| deal.value_cents)
        .sum();

    Ok(view! {
        <div class="page-head">
            <h1>(&company.name)</h1>
            <div class="actions">
                <a class="btn" href=(format!("/companies/{id}/edit"))>"Edit"</a>
                <form class="inline" method="post" action=(format!("/companies/{id}/delete"))>
                    csrf_field()
                    <button class="btn btn-danger" type="submit">"Delete"</button>
                </form>
            </div>
        </div>

        <div class="cols">
            <div>
                <div class="panel">
                    <dl class="meta">
                        <dt>"Industry"</dt>
                        <dd>(company.industry.as_deref().unwrap_or("—"))</dd>
                        <dt>"Website"</dt>
                        <dd>(company.website.as_deref().unwrap_or("—"))</dd>
                        <dt>"Phone"</dt>
                        <dd>(company.phone.as_deref().unwrap_or("—"))</dd>
                        <dt>"Notes"</dt>
                        <dd>(company.notes.as_deref().unwrap_or("—"))</dd>
                        <dt>"Open pipeline"</dt>
                        <dd>(domain::format_money(open_value))</dd>
                        <dt>"Added"</dt>
                        <dd>(zone.format_date(company.created_at))</dd>
                    </dl>
                </div>

                <h2>"Contacts"</h2>
                if contacts.is_empty() {
                    <p class="empty">"No contacts at this company yet."</p>
                } else {
                    <table>
                        <thead>
                            <tr><th>"Name"</th><th>"Title"</th><th>"Email"</th></tr>
                        </thead>
                        <tbody>
                            for contact in contacts {
                                <tr>
                                    <td>
                                        <a href=(format!("/contacts/{}", contact.id))>
                                            (domain::full_name(&contact.first_name, &contact.last_name))
                                        </a>
                                    </td>
                                    <td class="muted">(contact.title.as_deref().unwrap_or("—"))</td>
                                    <td class="muted">(contact.email.as_deref().unwrap_or("—"))</td>
                                </tr>
                            }
                        </tbody>
                    </table>
                }
                <p><a href=(format!("/contacts/new?company_id={id}"))>"Add a contact"</a></p>

                <h2>"Deals"</h2>
                if deals.is_empty() {
                    <p class="empty">"No deals at this company yet."</p>
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
                        company_id: Some(id),
                        contact_id: None,
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

#[page("/companies/{company_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    auth::require_user(cx)?;
    let id = *path_param::<CompanyId>(cx)?;
    let company = find(&mut db, id).await?;

    Ok(view! {
        <div class="page-head">
            <h1>"Edit company"</h1>
            <a class="btn" href=(format!("/companies/{id}"))>"Cancel"</a>
        </div>
        <div class="panel">
            company_form(
                action: format!("/companies/{id}"),
                name: company.name.clone(),
                industry: company.industry.clone().unwrap_or_default(),
                website: company.website.clone().unwrap_or_default(),
                phone: company.phone.clone().unwrap_or_default(),
                notes: company.notes.clone().unwrap_or_default(),
                submit_label: "Save changes",
            )
        </div>
    })
}

#[route(POST "/companies/{company_id}")]
async fn update(cx: &Cx, body: crate::csrf::CsrfForm<CompanyForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let crate::csrf::CsrfForm(input) = body;
    auth::require_user(cx)?;
    let id = *path_param::<CompanyId>(cx)?;

    let name = input.name.trim();
    if name.is_empty() {
        return Err(bad_request("Company name is required").into());
    }

    let mut company = find(&mut db, id).await?;
    toasty::update!(company {
        name,
        industry: domain::opt(input.industry),
        website: domain::opt(input.website),
        phone: domain::opt(input.phone),
        notes: domain::opt(input.notes),
    })
    .exec(&mut db)
    .await?;

    Ok(see_other(format!("/companies/{id}")))
}

// --- Delete ----------------------------------------------------------------

#[route(POST "/companies/{company_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = db(cx);
    auth::require_user(cx)?;
    let id = *path_param::<CompanyId>(cx)?;

    // Detach children instead of leaving dangling foreign keys behind.
    for mut contact in Contact::filter(Contact::fields().company_id().eq(id))
        .exec(&mut db)
        .await?
    {
        toasty::update!(contact { company_id: Option::<i64>::None })
            .exec(&mut db)
            .await?;
    }
    for mut deal in Deal::filter(Deal::fields().company_id().eq(id))
        .exec(&mut db)
        .await?
    {
        toasty::update!(deal { company_id: Option::<i64>::None })
            .exec(&mut db)
            .await?;
    }
    for mut activity in Activity::filter(Activity::fields().company_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(activity { company_id: Option::<i64>::None })
            .exec(&mut db)
            .await?;
    }

    Company::delete_by_id(&mut db, id).await?;

    Ok(see_other("/companies"))
}

// --- Shared form -----------------------------------------------------------

/// Create/edit form. Both pages pass every field as a string, so the same
/// markup serves an empty form and a filled one.
#[component]
async fn company_form(
    action: String,
    name: String,
    industry: String,
    website: String,
    phone: String,
    notes: String,
    submit_label: &str,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
            csrf_field()
            <div class="field">
                <label for="name">"Name"</label>
                <input id="name" name="name" value=(name) required="" autofocus="">
            </div>
            <div class="form-row">
                <div class="field">
                    <label for="industry">"Industry"</label>
                    <input id="industry" name="industry" value=(industry)>
                </div>
                <div class="field">
                    <label for="phone">"Phone"</label>
                    <input id="phone" name="phone" value=(phone)>
                </div>
            </div>
            <div class="field">
                <label for="website">"Website"</label>
                <input id="website" name="website" value=(website)>
            </div>
            <div class="field">
                <label for="notes">"Notes"</label>
                <textarea id="notes" name="notes">(notes)</textarea>
            </div>
            <button class="btn btn-primary" type="submit">(submit_label)</button>
        </form>
    })
}
