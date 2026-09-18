//! Dashboard: headline counts, pipeline breakdown, recent activity.
//!
//! # Where the numbers come from
//!
//! The pipeline figures are aggregated by the database, not by this process.
//! The version this replaces read every deal row into memory and summed them
//! with `Iterator::sum`, so the dashboard's cost grew with the size of the
//! pipeline and its memory use with the size of the book.
//!
//! Toasty's typed API has `COUNT` but no `SUM` or `GROUP BY`, so these are
//! written as backend SQL through [`toasty::sql`]. That is a deliberate trade:
//! a handful of small, self-contained queries that the database can plan with
//! an index, instead of unbounded work in the web process.
//!
//! The placeholders are PostgreSQL's `$1`, which is the only backend the server
//! runs on. See the README for why.

use std::collections::HashMap;

use toasty::stmt::Value;
use topcoat::{
    Result,
    context::Cx,
    router::{error::internal_server_error, page, query_params},
    view::{View, view},
};

use crate::auth;
use crate::db;
use crate::domain::{self, Stage};
use crate::models::{Activity, Company, Contact};
use crate::pagination::{self, Sort};
use crate::views::{activity_feed, pager};

/// Newest first, with the primary key as the ordering column: `created_at` is
/// not unique, and paginating on a non-unique key can repeat or skip rows.
const SORT: Sort = Sort::desc("id");

#[query_params(error = bad_request)]
struct DashboardQuery {
    next: Option<String>,
    prev: Option<String>,
}

/// One row of the pipeline-by-stage table.
struct StageRow {
    label: &'static str,
    count: i64,
    value_cents: i64,
}

/// What the database reports about the whole pipeline.
#[derive(Debug, Default)]
struct PipelineTotals {
    open_deals: i64,
    open_value_cents: i64,
    won_value_cents: i64,
}

#[page("/")]
async fn dashboard(cx: &Cx) -> Result<impl View> {
    auth::require_user(cx)?;
    let mut db = db(cx);
    let zone = crate::views::zone(cx);
    let page_size = crate::config_of(cx).page_size;

    let company_count = Company::all().count().exec(&mut db).await?;
    let contact_count = Contact::all().count().exec(&mut db).await?;
    let activity_total = Activity::all().count().exec(&mut db).await? as usize;

    let totals = pipeline_totals(&mut db).await?;

    // Grouped by stage in one query, then laid out in the canonical order so
    // the table reads the same way the pipeline flows.
    let by_stage = stage_totals(&mut db).await?;
    let rows: Vec<StageRow> = Stage::ALL
        .into_iter()
        .map(|stage| {
            let (count, value_cents) = by_stage.get(stage.as_str()).copied().unwrap_or((0, 0));
            StageRow {
                label: stage.label(),
                count,
                value_cents,
            }
        })
        .collect();

    // Recent activity, paginated like every other list rather than truncated at
    // eight rows with no way to see the rest.
    let params = query_params::<DashboardQuery>(cx)?;
    let offset = crate::pages::position(
        &crate::pages::Pagination {
            next: params.next.clone(),
            prev: params.prev.clone(),
        },
        SORT,
    )
    .offset;
    let recent = Activity::all()
        .order_by(Activity::fields().id().desc())
        .limit(pagination::fetch_limit(page_size))
        .offset(offset)
        .exec(&mut db)
        .await?;
    let recent_page =
        pagination::assemble(recent, page_size, offset, activity_total, SORT);
    let authors = crate::views::author_names(&mut db, &recent_page.rows).await?;
    let shown_from = recent_page.showing_from();
    let shown_to = recent_page.showing_to();

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
                <div class="value">(totals.open_deals)</div>
                <div class="label">"Open deals"</div>
            </div>
            <div class="stat">
                <div class="value">(domain::format_money(totals.open_value_cents))</div>
                <div class="label">"Pipeline value"</div>
            </div>
            <div class="stat">
                <div class="value">(domain::format_money(totals.won_value_cents))</div>
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

        <h2>"Activity"</h2>
        <div class="panel">
            activity_feed(
                activities: recent_page.rows,
                authors: authors,
                zone: zone,
            )
        </div>
        pager(
            base: "/",
            query: "",
            total: recent_page.total,
            shown_from: shown_from,
            shown_to: shown_to,
            prev: recent_page.prev.clone(),
            next: recent_page.next.clone(),
        )
    })
}

/// Counts and cash for the whole pipeline, in one pass over the deals table.
async fn pipeline_totals(db: &mut toasty::Db) -> Result<PipelineTotals> {
    // The two "won"/"lost" literals and the open set are bound as parameters
    // rather than interpolated, so the SQL text is a constant.
    let open_stages: Vec<String> = Stage::ALL
        .into_iter()
        .filter(|stage| stage.is_open())
        .map(|stage| stage.as_str().to_string())
        .collect();
    let open_markers = placeholders(open_stages.len(), 3);

    // `CAST(… AS BIGINT)` matters: PostgreSQL's `SUM` over a `BIGINT` returns
    // `NUMERIC`, which the driver cannot decode without its decimal feature.
    // Casting back to `BIGINT` keeps the arithmetic in the database and the
    // result in the range `value_cents` already uses.
    let sql = format!(
        "SELECT CAST(COUNT(*) AS BIGINT), \
         CAST(COALESCE(SUM(CASE WHEN stage IN ({open_markers}) THEN value_cents ELSE 0 END), 0) AS BIGINT), \
         CAST(COALESCE(SUM(CASE WHEN stage = $1 THEN value_cents ELSE 0 END), 0) AS BIGINT) \
         FROM deals"
    );

    let mut query = toasty::sql::query(sql)
        .bind(Stage::Won.as_str())
        .bind(Stage::Lost.as_str());
    for stage in &open_stages {
        query = query.bind(stage.clone());
    }

    let rows = query
        .column_types([
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
            toasty::stmt::Type::I64,
        ])
        .exec(db)
        .await
        .map_err(internal_server_error)?;

    let Some(record) = rows.first().and_then(record_fields) else {
        return Ok(PipelineTotals::default());
    };

    // The first column counts every deal, so the open figure is derived by
    // subtracting the two closed stages rather than counting twice.
    let total = int_at(record, 0);
    let open_value = int_at(record, 1);
    let won_value = int_at(record, 2);
    let closed = closed_count(db).await?;

    Ok(PipelineTotals {
        open_deals: total - closed,
        open_value_cents: open_value,
        won_value_cents: won_value,
    })
}

/// How many deals sit in a closed stage.
async fn closed_count(db: &mut toasty::Db) -> Result<i64> {
    let rows = toasty::sql::query(
        "SELECT CAST(COUNT(*) AS BIGINT) FROM deals WHERE stage IN ($1, $2)",
    )
    .bind(Stage::Won.as_str())
    .bind(Stage::Lost.as_str())
    .column_types([toasty::stmt::Type::I64])
    .exec(db)
    .await
    .map_err(internal_server_error)?;

    Ok(rows
        .first()
        .and_then(record_fields)
        .map(|record| int_at(record, 0))
        .unwrap_or(0))
}

/// Deal count and total value per stage, keyed by the stored stage string.
async fn stage_totals(db: &mut toasty::Db) -> Result<HashMap<String, (i64, i64)>> {
    let rows = toasty::sql::query(
        "SELECT stage, CAST(COUNT(*) AS BIGINT), \
         CAST(COALESCE(SUM(value_cents), 0) AS BIGINT) \
         FROM deals GROUP BY stage",
    )
    .column_types([
        toasty::stmt::Type::String,
        toasty::stmt::Type::I64,
        toasty::stmt::Type::I64,
    ])
    .exec(db)
    .await
    .map_err(internal_server_error)?;

    let mut totals = HashMap::new();
    for row in rows {
        let Some(record) = record_fields(&row) else {
            continue;
        };
        let Some(stage) = string_at(record, 0) else {
            continue;
        };
        totals.insert(stage, (int_at(record, 1), int_at(record, 2)));
    }
    Ok(totals)
}

/// `$3, $4, …`, starting at `first`.
fn placeholders(count: usize, first: usize) -> String {
    (first..first + count)
        .map(|index| format!("${index}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The fields of a raw-SQL row, whichever record shape the driver produced.
fn record_fields(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Record(record) => Some(&record.fields),
        Value::SparseRecord(record) => Some(record.values.as_slice()),
        Value::List(values) => Some(values),
        _ => None,
    }
}

/// Read an integer column, tolerating the wider types a driver may infer.
fn int_at(fields: &[Value], index: usize) -> i64 {
    match fields.get(index) {
        Some(Value::I64(value)) => *value,
        Some(Value::U64(value)) => *value as i64,
        Some(Value::U32(value)) => i64::from(*value),
        Some(Value::I32(value)) => i64::from(*value),
        Some(Value::F64(value)) => *value as i64,
        Some(Value::F32(value)) => *value as i64,
        Some(Value::Null) | None => 0,
        _ => 0,
    }
}

/// Read a text column.
fn string_at(fields: &[Value], index: usize) -> Option<String> {
    match fields.get(index) {
        Some(Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_numbered_from_the_given_index() {
        assert_eq!(placeholders(0, 3), "");
        assert_eq!(placeholders(1, 3), "$3");
        assert_eq!(placeholders(4, 3), "$3, $4, $5, $6");
    }

    #[test]
    fn integers_are_read_across_the_numeric_variants() {
        let fields = vec![
            Value::I64(7),
            Value::U64(8),
            Value::I32(-2),
            Value::F64(3.9),
            Value::Null,
        ];
        assert_eq!(int_at(&fields, 0), 7);
        assert_eq!(int_at(&fields, 1), 8);
        assert_eq!(int_at(&fields, 2), -2);
        assert_eq!(int_at(&fields, 3), 3);
        assert_eq!(int_at(&fields, 4), 0);
        assert_eq!(int_at(&fields, 9), 0);
    }

    #[test]
    fn engine_errors_become_a_bare_500() {
        // A raw-SQL failure carries a database message naming tables and
        // values; `internal_server_error` keeps that on the server side and
        // renders a bare status line, which is what `Display` exposes.
        let error = internal_server_error(anyhow::anyhow!("relation \"deals\" does not exist"));
        assert_eq!(error.to_string(), "internal server error");
    }
}
