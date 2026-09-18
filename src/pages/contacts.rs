//! Contact pages: list, create, detail, edit, delete.

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

path_param!(contact_id: u64, error = bad_request("Contact id must be a number"));

#[query_params(error = bad_request)]
struct Search {
    q: Option<String>,
}

#[query_params(error = bad_request)]
struct NewContact {
    company_id: Option<u64>,
}

/// One row of the contact list.
struct Row {
    id: u64,
    name: String,
    title: String,
    email: String,
    phone: String,
    company: String,
}

/// Load a contact or answer 404.
async fn find(db: &mut toasty::Db, id: u64) -> Result<Contact> {
    Contact::filter(Contact::fields().id().eq(id))
        .first()
        .exec(db)
        .await?
        .ok_or_not_found()
        .map_err(Into::into)
}

/// `(id, name)` for every company, for the company picker.
async fn company_options(db: &mut toasty::Db) -> Result<Vec<(u64, String)>> {
    Ok(Company::all()
        .order_by(Company::fields().name().asc())
        .exec(db)
        .await?
        .into_iter()
        .map(|company| (company.id, company.name))
        .collect())
}

// --- List ------------------------------------------------------------------

#[page("/contacts")]
async fn index(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);

    let search = query_params::<Search>(cx)?
        .q
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();

    let contacts = if search.is_empty() {
        Contact::all()
            .order_by(Contact::fields().last_name().asc())
            .exec(&mut db)
            .await?
    } else {
        let pattern = format!("%{search}%");
        Contact::filter(
            Contact::fields()
                .first_name()
                .like(pattern.clone())
                .or(Contact::fields().last_name().like(pattern.clone()))
                .or(Contact::fields().email().like(pattern.clone()))
                .or(Contact::fields().title().like(pattern)),
        )
        .order_by(Contact::fields().last_name().asc())
        .exec(&mut db)
        .await?
    };

    let companies = Company::all().exec(&mut db).await?;
    let rows: Vec<Row> = contacts
        .into_iter()
        .map(|contact| Row {
            id: contact.id,
            name: domain::full_name(&contact.first_name, &contact.last_name),
            title: contact.title.unwrap_or_default(),
            email: contact.email.unwrap_or_default(),
            phone: contact.phone.unwrap_or_default(),
            company: contact
                .company_id
                .and_then(|id| companies.iter().find(|c| c.id == id))
                .map(|c| c.name.clone())
                .unwrap_or_default(),
        })
        .collect();

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
    })
}

// --- Create ----------------------------------------------------------------

#[page("/contacts/new")]
async fn new_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let preselected = query_params::<NewContact>(cx)?.company_id;
    let options = company_options(&mut db).await?;

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
async fn create(cx: &Cx, Form(input): Form<ContactForm>) -> Result<SeeOther> {
    let first_name = input.first_name.trim();
    let last_name = input.last_name.trim();
    if first_name.is_empty() && last_name.is_empty() {
        return Err(bad_request("A first or last name is required").into());
    }

    let contact = toasty::create!(Contact {
        first_name,
        last_name,
        email: opt(input.email),
        phone: opt(input.phone),
        title: opt(input.title),
        company_id: opt(input.company_id).and_then(|id| id.parse().ok()),
        notes: opt(input.notes),
        created_at: domain::now(),
    })
    .exec(&mut db(cx))
    .await?;

    Ok(see_other(format!("/contacts/{}", contact.id)))
}

// --- Detail ----------------------------------------------------------------

#[page("/contacts/{contact_id}")]
async fn show(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let id = *path_param::<ContactId>(cx)?;
    let contact = find(&mut db, id).await?;

    let company = match contact.company_id {
        Some(company_id) => Company::filter(Company::fields().id().eq(company_id))
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
    let deals = Deal::filter(Deal::fields().contact_id().eq(Some(id)))
        .order_by(Deal::fields().created_at().desc())
        .exec(&mut db)
        .await?;
    let activities = Activity::filter(Activity::fields().contact_id().eq(Some(id)))
        .order_by(Activity::fields().created_at().desc())
        .exec(&mut db)
        .await?;

    Ok(view! {
        <div class="page-head">
            <h1>(domain::full_name(&contact.first_name, &contact.last_name))</h1>
            <div class="actions">
                <a class="btn" href=(format!("/contacts/{id}/edit"))>"Edit"</a>
                <form class="inline" method="post" action=(format!("/contacts/{id}/delete"))>
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
                        <dd>(domain::format_date(contact.created_at))</dd>
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
                    activity_feed(activities: activities)
                </div>
            </div>
        </div>
    })
}

// --- Edit ------------------------------------------------------------------

#[page("/contacts/{contact_id}/edit")]
async fn edit_form(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);
    let id = *path_param::<ContactId>(cx)?;
    let contact = find(&mut db, id).await?;
    let options = company_options(&mut db).await?;

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
async fn update(cx: &Cx, Form(input): Form<ContactForm>) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<ContactId>(cx)?;

    let first_name = input.first_name.trim();
    let last_name = input.last_name.trim();
    if first_name.is_empty() && last_name.is_empty() {
        return Err(bad_request("A first or last name is required").into());
    }

    let mut contact = find(&mut db, id).await?;
    toasty::update!(contact {
        first_name,
        last_name,
        email: opt(input.email),
        phone: opt(input.phone),
        title: opt(input.title),
        company_id: opt(input.company_id).and_then(|id| id.parse().ok()),
        notes: opt(input.notes),
    })
    .exec(&mut db)
    .await?;

    Ok(see_other(format!("/contacts/{id}")))
}

// --- Delete ----------------------------------------------------------------

#[route(POST "/contacts/{contact_id}/delete")]
async fn destroy(cx: &Cx) -> Result<SeeOther> {
    let mut db = db(cx);
    let id = *path_param::<ContactId>(cx)?;

    for mut deal in Deal::filter(Deal::fields().contact_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(deal { contact_id: Option::<u64>::None })
            .exec(&mut db)
            .await?;
    }
    for mut activity in Activity::filter(Activity::fields().contact_id().eq(Some(id)))
        .exec(&mut db)
        .await?
    {
        toasty::update!(activity { contact_id: Option::<u64>::None })
            .exec(&mut db)
            .await?;
    }

    Contact::delete_by_id(&mut db, id).await?;

    Ok(see_other("/contacts"))
}

// --- Shared form -----------------------------------------------------------

#[component]
async fn company_select(companies: Vec<(u64, String)>, selected: Option<u64>) -> Result<impl View> {
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
    companies: Vec<(u64, String)>,
    selected_company: Option<u64>,
    submit_label: &str,
) -> Result<impl View> {
    Ok(view! {
        <form method="post" action=(action)>
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
