//! The demo book.
//!
//! There is no bootstrap account any more. A fresh installation is entered
//! through `/signup`, which creates a workspace and makes its first member an
//! administrator — so the app never ships a shared credential, and never has a
//! user without a workspace.
//!
//! [`seed_demo_data`] gives the *first* workspace some records to look at: it
//! inserts the sample companies, contacts, deals, and activities, but only when
//! the database has no companies at all. Set `CRM_SEED=0` to skip it.

use crate::domain::{self, ActivityKind, Stage, TimeZone};
use crate::models::{Activity, Company, Contact, Deal, User};
use toasty::Db;

/// Shorthand for an optional text field.
fn s(value: &str) -> Option<String> {
    Some(value.to_string())
}

/// Unix seconds `days` days ago.
fn days_ago(days: i64) -> i64 {
    domain::now() - days * domain::DAY
}

/// Unix seconds `days` days from now, at the start of that day in `zone`.
///
/// Expected close dates go through the same
/// [`crate::domain::parse_date_in`] the form uses, so a demo row renders back
/// as the date it was seeded for rather than shifting by the offset.
fn days_ahead(days: i64, zone: &TimeZone) -> i64 {
    let target = domain::now() + days * domain::DAY;
    zone.start_of_day(target).unwrap_or(target)
}

/// The workspace and member demo data is attributed to, if there is one.
///
/// The demo book needs a tenant, and it is only meaningful once somebody has
/// signed up. With no account yet there is nothing to seed, so startup skips it
/// and the first workspace starts empty.
async fn demo_owner(db: &mut Db) -> Option<(i64, Option<i64>)> {
    let user = User::all()
        .order_by(User::fields().id().asc())
        .first()
        .exec(db)
        .await
        .ok()
        .flatten()?;
    Some((user.account_id, Some(user.id)))
}

/// Insert the demo book into the first workspace, unless there is already data.
///
/// Returns whether anything was written, so startup can say which of the two
/// reasons it skipped.
pub async fn seed_demo_data(db: &Db, zone: &TimeZone) -> toasty::Result<bool> {
    // `Db` is a cheap handle to the pool, so the clone is not a second pool.
    let mut db = db.clone();

    if Company::all().count().exec(&mut db).await? > 0 {
        return Ok(false);
    }

    // No workspace means nobody has signed up yet: there is no tenant to put
    // the demo rows in, and inventing one would create a workspace nobody can
    // sign in to.
    let Some((account_id, author)) = demo_owner(&mut db).await else {
        return Ok(false);
    };

    // --- Companies ---------------------------------------------------------

    let northwind = toasty::create!(Company {
        account_id,
        name: "Northwind Trading",
        industry: s("Import / Export"),
        website: s("https://northwind.example.com"),
        phone: s("+1 415 555 0141"),
        notes: s("Long-standing account; contract renews in Q1."),
        created_at: days_ago(210),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let acme = toasty::create!(Company {
        account_id,
        name: "Acme Robotics",
        industry: s("Manufacturing"),
        website: s("https://acme-robotics.example.com"),
        phone: s("+1 206 555 0188"),
        notes: s("Two business units; procurement is centralised in Seattle."),
        created_at: days_ago(160),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let blue_harbor = toasty::create!(Company {
        account_id,
        name: "Blue Harbor Logistics",
        industry: s("Logistics"),
        website: s("https://blueharbor.example.com"),
        phone: s("+1 503 555 0173"),
        notes: None,
        created_at: days_ago(95),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let vertex = toasty::create!(Company {
        account_id,
        name: "Vertex Analytics",
        industry: s("Software"),
        website: s("https://vertex.example.com"),
        phone: s("+1 617 555 0110"),
        notes: s("Inbound lead from the March webinar."),
        created_at: days_ago(40),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    // A company whose name carries the characters a `LIKE` search used to treat
    // as wildcards, so the escape path is exercised by ordinary use.
    let discount = toasty::create!(Company {
        account_id,
        name: "50% Off Supplies",
        industry: s("Retail"),
        website: None,
        phone: None,
        notes: s("Name contains a literal percent sign, on purpose."),
        created_at: days_ago(5),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    // --- Contacts ----------------------------------------------------------

    let turing = toasty::create!(Contact {
        account_id,
        first_name: "Alan",
        last_name: "Turing",
        email: s("alan.turing@northwind.example.com"),
        phone: s("+1 415 555 0142"),
        title: s("Head of Operations"),
        company_id: Some(northwind.id),
        notes: None,
        created_at: days_ago(200),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let hopper = toasty::create!(Contact {
        account_id,
        first_name: "Grace",
        last_name: "Hopper",
        email: s("grace.hopper@acme-robotics.example.com"),
        phone: s("+1 206 555 0189"),
        title: s("VP Engineering"),
        company_id: Some(acme.id),
        notes: s("Prefers email over calls."),
        created_at: days_ago(150),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let pauling = toasty::create!(Contact {
        account_id,
        first_name: "Linus",
        last_name: "Pauling",
        email: s("linus.pauling@acme-robotics.example.com"),
        phone: None,
        title: s("CFO"),
        company_id: Some(acme.id),
        notes: None,
        created_at: days_ago(120),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let johnson = toasty::create!(Contact {
        account_id,
        first_name: "Katherine",
        last_name: "Johnson",
        email: s("katherine.johnson@blueharbor.example.com"),
        phone: s("+1 503 555 0174"),
        title: s("Procurement Lead"),
        company_id: Some(blue_harbor.id),
        notes: None,
        created_at: days_ago(90),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let lovelace = toasty::create!(Contact {
        account_id,
        first_name: "Ada",
        last_name: "Lovelace",
        email: s("ada.lovelace@vertex.example.com"),
        phone: s("+1 617 555 0111"),
        title: s("CTO"),
        company_id: Some(vertex.id),
        notes: s("Technical buyer; wants a security review before signing."),
        created_at: days_ago(38),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    // A contact that is not attached to a company yet.
    toasty::create!(Contact {
        account_id,
        first_name: "Rosalind",
        last_name: "Franklin",
        email: s("rosalind@example.com"),
        phone: None,
        title: s("Independent consultant"),
        company_id: None,
        notes: s("Met at the logistics expo."),
        created_at: days_ago(12),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    toasty::create!(Contact {
        account_id,
        first_name: "Barbara",
        last_name: "Liskov",
        email: s("barbara@offsupplies.example.com"),
        phone: None,
        title: s("Buyer"),
        company_id: Some(discount.id),
        notes: None,
        created_at: days_ago(4),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    // --- Deals -------------------------------------------------------------

    let freight_portal = toasty::create!(Deal {
        account_id,
        title: "Freight portal build-out",
        value_cents: 3_250_000,
        stage: Stage::Qualified.as_str(),
        company_id: Some(northwind.id),
        contact_id: Some(turing.id),
        expected_close: Some(days_ahead(45, zone)),
        notes: s("Scope agreed; waiting on their security questionnaire."),
        created_at: days_ago(60),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let line_retrofit = toasty::create!(Deal {
        account_id,
        title: "Assembly line retrofit",
        value_cents: 12_500_000,
        stage: Stage::Proposal.as_str(),
        company_id: Some(acme.id),
        contact_id: Some(hopper.id),
        expected_close: Some(days_ahead(30, zone)),
        notes: s("Proposal sent; procurement review scheduled."),
        created_at: days_ago(50),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    toasty::create!(Deal {
        account_id,
        title: "Support renewal 2027",
        value_cents: 2_200_000,
        stage: Stage::Won.as_str(),
        company_id: Some(acme.id),
        contact_id: Some(pauling.id),
        expected_close: Some(days_ago(14)),
        notes: s("Signed at list price."),
        created_at: days_ago(80),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let tracking_addon = toasty::create!(Deal {
        account_id,
        title: "Fleet tracking add-on",
        value_cents: 1_800_000,
        stage: Stage::Lead.as_str(),
        company_id: Some(blue_harbor.id),
        contact_id: Some(johnson.id),
        expected_close: Some(days_ahead(75, zone)),
        notes: None,
        created_at: days_ago(20),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    let platform_licence = toasty::create!(Deal {
        account_id,
        title: "Analytics platform licence",
        value_cents: 4_800_000,
        stage: Stage::Negotiation.as_str(),
        company_id: Some(vertex.id),
        contact_id: Some(lovelace.id),
        expected_close: Some(days_ahead(18, zone)),
        notes: s("Negotiating multi-year discount."),
        created_at: days_ago(35),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    toasty::create!(Deal {
        account_id,
        title: "Pilot expansion",
        value_cents: 950_000,
        stage: Stage::Lost.as_str(),
        company_id: Some(vertex.id),
        contact_id: Some(lovelace.id),
        expected_close: Some(days_ago(7)),
        notes: s("Lost to an incumbent vendor on price."),
        created_at: days_ago(70),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    toasty::create!(Deal {
        account_id,
        title: "Bulk packaging order",
        value_cents: 480_000,
        stage: Stage::Lead.as_str(),
        company_id: Some(discount.id),
        contact_id: None,
        expected_close: Some(days_ahead(10, zone)),
        notes: None,
        created_at: days_ago(3),
        created_by: author,
    })
    .exec(&mut db)
    .await?;

    // --- Activities --------------------------------------------------------

    let activities = [
        (
            ActivityKind::Meeting,
            "Kick-off with their ops team; walked through the integration plan.",
            Some(northwind.id),
            Some(turing.id),
            Some(freight_portal.id),
            58,
        ),
        (
            ActivityKind::Email,
            "Sent the security questionnaire back, signed.",
            Some(northwind.id),
            Some(turing.id),
            Some(freight_portal.id),
            41,
        ),
        (
            ActivityKind::Call,
            "Grace confirmed budget is approved for the retrofit.",
            Some(acme.id),
            Some(hopper.id),
            Some(line_retrofit.id),
            33,
        ),
        (
            ActivityKind::Note,
            "Proposal v2 sent with the 3-year support option.",
            Some(acme.id),
            Some(hopper.id),
            Some(line_retrofit.id),
            12,
        ),
        (
            ActivityKind::Email,
            "Introduced ourselves and shared the fleet tracking datasheet.",
            Some(blue_harbor.id),
            Some(johnson.id),
            Some(tracking_addon.id),
            19,
        ),
        (
            ActivityKind::Meeting,
            "Security review with Ada and their platform team.",
            Some(vertex.id),
            Some(lovelace.id),
            Some(platform_licence.id),
            9,
        ),
        (
            ActivityKind::Call,
            "Negotiation call; they asked for 15% off for a 3-year term.",
            Some(vertex.id),
            Some(lovelace.id),
            Some(platform_licence.id),
            5,
        ),
        (
            ActivityKind::Note,
            "Met at the logistics expo; possible consultant referral.",
            None,
            None,
            None,
            11,
        ),
    ];

    for (kind, body, company_id, contact_id, deal_id, age) in activities {
        toasty::create!(Activity {
            account_id,
            kind: kind.as_str(),
            body,
            company_id,
            contact_id,
            deal_id,
            created_at: days_ago(age),
            user_id: author,
        })
        .exec(&mut db)
        .await?;
    }

    Ok(true)
}


