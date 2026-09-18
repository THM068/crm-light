//! Deal pages: pipeline list, create, detail, stage moves, edit, delete.

use crate::db;
use crate::domain::{self, Stage, opt, parse_date, parse_money};
use crate::models::{Activity, Company, Contact, Deal};
use crate::pages::{activity_feed, activity_form, stage_badge};
use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::{RouterErrorExt, SeeOther, bad_request, see_other},
        page, path_param, query_params, route,
    },
    view::{View, component, view},
};

path_param!(deal_id: u64, error = bad_request("Deal id must be a number"));

#[query_params(error = bad_request)]
struct Filters {
    q: Option<String>,
    stage: Option<String>,
}

/// One row of the pipeline list.
struct Row {
    id: u64,
    title: String,
    stage: String,
    value_cents: i64,
    company: String,
    contact: String,
    close: String,
}

/// Load a deal or answer 404.
async fn find(db: &mut toasty::Db, id: u64) -> Result<Deal> {
    Deal::filter(Deal::fields().id().eq(id))
        .first()
        .exec(db)
        .await?
        .ok_or_not_found()
        .map_err(Into::into)
}

/// `(id, label)` pairs for the company picker.
async fn company_options(db: &mut toasty::Db) -> Result<Vec<(u64, String)>> {
    Ok(Company::all()
        .order_by(Company::fields().name().asc())
        .exec(db)
        .await?
        .into_iter()
        .map(|company| (company.id, company.name))
        .collect())
}

/// `(id, label)` pairs for the contact picker.
async fn contact_options(db: &mut toasty::Db) -> Result<Vec<(u64, String)>> {
    Ok(Contact::all()
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

    let filters = query_params::<Filters>(cx)?;
    let search = filters.q.clone().unwrap_or_default().trim().to_string();
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

    let mut query = Deal::all().order_by(Deal::fields().created_at().desc());
    if !search.is_empty() {
        query = query.filter(Deal::fields().title().like(format!("%{search}%")));
    }
    if stage_active {
        query = query.filter(Deal::fields().stage().eq(stage_filter.as_str()));
    }
    let deals = query.exec(&mut db).await?;

    let companies = Company::all().exec(&mut db).await?;
    let contacts = Contact::all().exec(&mut db).await?;

    let total: i64 = deals
        .iter()
        .filter(|deal| Stage::from_stored(&deal.stage).is_open())
        .map(|deal| deal.value_cents)
        .sum();

    let rows: Vec<Row> = deals
        .into_iter()
        .map(|deal| Row {
            company: deal
                .company_id
                .and_then(|id| companies.iter().find(|c| c.id == id))
                .map(|c| c.name.clone())
                .unwrap_or_default(),
            contact: deal
                .contact_id
                .and_then(|id| contacts.iter().find(|c| c.id == id))
                .map(|c| domain::full_name(&c.first_name, &c.last_name))
                .unwrap_or_default(),
            close: deal
                .expected_close
                .map(domain::format_date)
                .unwrap_or_default(),
            id: deal.id,
            title: deal.title,
            stage: deal.stage,
            value_cents: deal.value_cents,
        })
        .collect();

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
                "Open pipeline shown: "
                (domain::format_money(total))
            </p>
        }
    })
}

// --- Create ----------------------------------------------------------------

#[page("/deals/new")]
async fn new_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let companies = company_options(&mut db).await?;
    let contacts = contact_options(&mut db).await?;

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
fn form_id(value: String) -> Option<u64> {
    opt(value).and_then(|id| id.parse().ok())
}

/// Read a `value` form field: empty means zero.
fn form_money(value: String) -> i64 {
    opt(value).and_then(|v| parse_money(&v)).unwrap_or(0)
}

#[route(POST "/deals")]
async fn create(cx: &Cx, Form(input): Form<DealForm>) -> Result<SeeOther> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(bad_request("Deal title is required").into());
    }

    let deal = toasty::create!(Deal {
        title,
        value_cents: form_money(input.value),
        stage: Stage::parse(input.stage.trim()).unwrap_or(Stage::Lead).as_str(),
        company_id: form_id(input.company_id),
        contact_id: form_id(input.contact_id),
        expected_close: opt(input.expected_close).and_then(|d| parse_date(&d)),
        notes: opt(input.notes),
        created_at: domain::now(),
    })
    .exec(&mut db(cx))
    .await?;

    Ok(see_other(format!("/deals/{}", deal.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/deals/{deal_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let id = *path_param::<DealId>(cx)?;
    let deal = find(&mut db, id).await?;

    let company = match deal.company_id {
        Some(company_id) => Company::filter(Company::fields().id().eq(company_id))
            .first()
            .exec(&mut db)
            .await?,
        None => None,
    };
    let contact = match deal.contact_id {
        Some(contact_id) => Contact::filter(Contact::fields().id().eq(contact_id))
            .first()
            .exec(&mut db)
            .await?,
        None => None,
    };
    let activities = Activity::filter(Activity::fields().deal_id().eq(Some(id)))
        .order_by(Activity::fields().created_at().desc())
        .exec(&mut db)
        .await?;

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
                        <dd>(deal.expected_close.map(domain::format_date).unwrap_or_else(|| "—".to_string()))</dd>
                        <dt>"Notes"</dt>
                        <dd>(deal.notes.as_deref().unwrap_or("—"))</dd>
                        <dt>"Created"</dt>
                        <dd>(domain::format_date(deal.created_at))</dd>
                    </dl>
                </div>

                <h2>"Move stage"</h2>
                <div class="panel">
                    <form method="post" action=(format!("/deals/{id}/stage"))>
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
                    activity_feed(activities: activities)
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
async fn set_stage(cx: &Cx, Form(input): Form<StageForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<DealId>(cx)?;
    let stage = Stage::parse(input.stage.trim())
        .ok_or_else(|| bad_request("Unknown stage"))?;

    let mut deal = find(&mut db, id).await?;
    toasty::update!(deal { stage: stage.as_str() })
        .exec(&mut db)
        .await?;

    Ok(see_other(format!("/deals/{id}")))
}

// --- Edit ------------------------------------------------------------------

#[page("/deals/{deal_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let id = *path_param::<DealId>(cx)?;
    let deal = find(&mut db, id).await?;
    let companies = company_options(&mut db).await?;
    let contacts = contact_options(&mut db).await?;

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
                expected_close: domain::date_input_value(deal.expected_close),
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
async fn update(cx: &Cx, Form(input): Form<DealForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<DealId>(cx)?;

    let title = input.title.trim();
    if title.is_empty() {
        return Err(bad_request("Deal title is required").into());
    }

    let mut deal = find(&mut db, id).await?;
    toasty::update!(deal {
        title,
        value_cents: form_money(input.value),
        stage: Stage::parse(input.stage.trim()).unwrap_or(Stage::Lead).as_str(),
        company_id: form_id(input.company_id),
        contact_id: form_id(input.contact_id),
        expected_close: opt(input.expected_close).and_then(|d| parse_date(&d)),
        notes: opt(input.notes),
    })
    .exec(&mut db)
    .await?;

    Ok(see_other(format!("/deals/{id}")))
}

// --- Delete ----------------------------------------------------------------

#[route(POST "/deals/{deal_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<DealId>(cx)?;

    for mut activity in Activity::filter(Activity::fields().deal_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(activity { deal_id: Option::<u64>::None })
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
    options: Vec<(u64, String)>,
    selected: Option<u64>,
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
    companies: Vec<(u64, String)>,
    contacts: Vec<(u64, String)>,
    selected_company: Option<u64>,
    selected_contact: Option<u64>,
    submit_label: &str,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
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
