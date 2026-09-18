//! Company pages: list, create, detail, edit, delete.

use crate::db;
use crate::domain::{self, opt};
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

path_param!(company_id: u64, error = bad_request("Company id must be a number"));

#[query_params(error = bad_request)]
struct Search {
    q: Option<String>,
}

/// One row of the company list.
struct Row {
    id: u64,
    name: String,
    industry: String,
    contacts: usize,
    open_value_cents: i64,
}

/// Load a company or answer 404.
async fn find(db: &mut toasty::Db, id: u64) -> Result<Company> {
    Company::filter(Company::fields().id().eq(id))
        .first()
        .exec(db)
        .await?
        .ok_or_not_found()
        .map_err(Into::into)
}

// --- List ------------------------------------------------------------------

#[page("/companies")]
async fn index(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);

    let search = query_params::<Search>(cx)?
        .q
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();

    let companies = if search.is_empty() {
        Company::all()
            .order_by(Company::fields().name().asc())
            .exec(&mut db)
            .await?
    } else {
        Company::filter(Company::fields().name().like(format!("%{search}%")))
            .order_by(Company::fields().name().asc())
            .exec(&mut db)
            .await?
    };

    // Load the children once and group them in memory rather than issuing a
    // query per company.
    let contacts = Contact::all().exec(&mut db).await?;
    let deals = Deal::all().exec(&mut db).await?;

    let rows: Vec<Row> = companies
        .into_iter()
        .map(|company| Row {
            contacts: contacts
                .iter()
                .filter(|contact| contact.company_id == Some(company.id))
                .count(),
            open_value_cents: deals
                .iter()
                .filter(|deal| {
                    deal.company_id == Some(company.id)
                        && domain::Stage::from_stored(&deal.stage).is_open()
                })
                .map(|deal| deal.value_cents)
                .sum(),
            id: company.id,
            name: company.name,
            industry: company.industry.unwrap_or_default(),
        })
        .collect();

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
    })
}

// --- Create ----------------------------------------------------------------

#[page("/companies/new")]
async fn new_form() -> Result<impl View> {
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
async fn create(cx: &Cx, Form(input): Form<CompanyForm>) -> Result<SeeOther> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(bad_request("Company name is required").into());
    }

    let company = toasty::create!(Company {
        name,
        industry: opt(input.industry),
        website: opt(input.website),
        phone: opt(input.phone),
        notes: opt(input.notes),
        created_at: domain::now(),
    })
    .exec(&mut db(cx))
    .await?;

    Ok(see_other(format!("/companies/{}", company.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/companies/{company_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let id = *path_param::<CompanyId>(cx)?;
    let company = find(&mut db, id).await?;

    let contacts = Contact::filter(Contact::fields().company_id().eq(Some(id)))
        .order_by(Contact::fields().last_name().asc())
        .exec(&mut db)
        .await?;
    let deals = Deal::filter(Deal::fields().company_id().eq(Some(id)))
        .order_by(Deal::fields().created_at().desc())
        .exec(&mut db)
        .await?;
    let activities = Activity::filter(Activity::fields().company_id().eq(Some(id)))
        .order_by(Activity::fields().created_at().desc())
        .exec(&mut db)
        .await?;

    Ok(view! {
        <div class="page-head">
            <h1>(&company.name)</h1>
            <div class="actions">
                <a class="btn" href=(format!("/companies/{id}/edit"))>"Edit"</a>
                <form class="inline" method="post" action=(format!("/companies/{id}/delete"))>
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
                        <dt>"Added"</dt>
                        <dd>(domain::format_date(company.created_at))</dd>
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
                    activity_feed(activities: activities)
                </div>
            </div>
        </div>
    })
}

// --- Edit ------------------------------------------------------------------

#[page("/companies/{company_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
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
async fn update(cx: &Cx, Form(input): Form<CompanyForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<CompanyId>(cx)?;

    let name = input.name.trim();
    if name.is_empty() {
        return Err(bad_request("Company name is required").into());
    }

    let mut company = find(&mut db, id).await?;
    toasty::update!(company {
        name,
        industry: opt(input.industry),
        website: opt(input.website),
        phone: opt(input.phone),
        notes: opt(input.notes),
    })
    .exec(&mut db)
    .await?;

    Ok(see_other(format!("/companies/{id}")))
}

// --- Delete ----------------------------------------------------------------

#[route(POST "/companies/{company_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<CompanyId>(cx)?;

    // Detach children instead of leaving dangling foreign keys behind.
    for mut contact in Contact::filter(Contact::fields().company_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(contact { company_id: Option::<u64>::None })
            .exec(&mut db)
            .await?;
    }
    for mut deal in Deal::filter(Deal::fields().company_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(deal { company_id: Option::<u64>::None })
            .exec(&mut db)
            .await?;
    }
    for mut activity in Activity::filter(Activity::fields().company_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(activity { company_id: Option::<u64>::None })
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
