//! Dashboard: headline counts, pipeline breakdown, and recent activity.

use crate::db;
use crate::domain::{self, Stage};
use crate::models::{Activity, Company, Contact, Deal};
use crate::pages::activity_feed;
use topcoat::{
    Result,
    context::Cx,
    router::page,
    view::{View, view},
};

/// One row of the pipeline-by-stage table.
struct StageRow {
    label: &'static str,
    count: usize,
    value_cents: i64,
}

#[page("/")]
async fn dashboard(cx: &Cx) -> Result<impl View> {
    let mut db = db(cx);

    let company_count = Company::all().count().exec(&mut db).await?;
    let contact_count = Contact::all().count().exec(&mut db).await?;
    let deals = Deal::all()
        .order_by(Deal::fields().created_at().desc())
        .exec(&mut db)
        .await?;
    let recent = Activity::all()
        .order_by(Activity::fields().created_at().desc())
        .limit(8)
        .exec(&mut db)
        .await?;

    let open_count = deals
        .iter()
        .filter(|deal| Stage::from_stored(&deal.stage).is_open())
        .count();
    let pipeline_value: i64 = deals
        .iter()
        .filter(|deal| Stage::from_stored(&deal.stage).is_open())
        .map(|deal| deal.value_cents)
        .sum();
    let won_value: i64 = deals
        .iter()
        .filter(|deal| Stage::from_stored(&deal.stage) == Stage::Won)
        .map(|deal| deal.value_cents)
        .sum();

    let rows: Vec<StageRow> = Stage::ALL
        .into_iter()
        .map(|stage| {
            let in_stage: Vec<&Deal> = deals
                .iter()
                .filter(|deal| Stage::from_stored(&deal.stage) == stage)
                .collect();
            StageRow {
                label: stage.label(),
                count: in_stage.len(),
                value_cents: in_stage.iter().map(|deal| deal.value_cents).sum(),
            }
        })
        .collect();

    Ok(view! {
        <div class="page-head">
            <h1>"Dashboard"</h1>
            <a class="btn btn-primary" href="/deals/new">"New deal"</a>
        </div>

        <div class="grid">
            <div class="stat">
                <div class="value">(company_count)</div>
                <div class="label">"Companies"</div>
            </div>
            <div class="stat">
                <div class="value">(contact_count)</div>
                <div class="label">"Contacts"</div>
            </div>
            <div class="stat">
                <div class="value">(open_count)</div>
                <div class="label">"Open deals"</div>
            </div>
            <div class="stat">
                <div class="value">(domain::format_money(pipeline_value))</div>
                <div class="label">"Pipeline value"</div>
            </div>
            <div class="stat">
                <div class="value">(domain::format_money(won_value))</div>
                <div class="label">"Won"</div>
            </div>
        </div>

        <h2>"Pipeline by stage"</h2>
        <table>
            <thead>
                <tr>
                    <th>"Stage"</th>
                    <th class="num">"Deals"</th>
                    <th class="num">"Value"</th>
                </tr>
            </thead>
            <tbody>
                for row in rows {
                    <tr>
                        <td>(row.label)</td>
                        <td class="num">(row.count)</td>
                        <td class="num">(domain::format_money(row.value_cents))</td>
                    </tr>
                }
            </tbody>
        </table>

        <h2>"Recent activity"</h2>
        <div class="panel">
            activity_feed(activities: recent)
        </div>
    })
}
