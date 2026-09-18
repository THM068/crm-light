//! Page handlers.
//!
//! One module per URL family, plus `login` for the authentication surface and
//! `admin` for account management. Shared components live in [`crate::views`].

pub mod activities;
pub mod admin;
pub mod companies;
pub mod contacts;
pub mod dashboard;
pub mod deals;
pub mod login;
pub mod signup;

use std::collections::HashMap;

use toasty::stmt::Expr;
use topcoat::context::Cx;

use crate::auth;
use crate::domain::TimeZone;
use crate::models::Activity;
use crate::pagination::{self, Position, Sort};
use crate::views;

/// Query parameters shared by every list page: the pager's cursors.
///
/// Each page flattens these fields into its own `#[query_params]` struct, so
/// the whole query-string contract stays visible where the page is defined,
/// while `position` below keeps the pager's rules in one place.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Pagination {
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub prev: Option<String>,
}

impl Pagination {
    /// The cursor parameters as [`crate::pagination`] understands them.
    fn cursors(&self) -> pagination::CursorParams {
        pagination::CursorParams {
            next: self.next.clone(),
            prev: self.prev.clone(),
        }
    }
}

/// Where a request sits in a list sorted by `sort`.
///
/// A cursor that does not match `sort` is ignored rather than rejected, so a
/// bookmarked link from before an ordering change lands on the first page
/// instead of on an error.
pub fn position(cursors: &Pagination, sort: Sort) -> Position {
    pagination::resolve(&cursors.cursors(), sort)
}

/// Everything a detail page's activity panel needs.
pub struct ActivityPanel {
    pub activities: Vec<Activity>,
    pub authors: HashMap<i64, String>,
    pub zone: TimeZone,
}

/// Load the activity feed for one association and resolve its author names.
pub async fn activity_panel(
    db: &mut toasty::Db,
    filter: Expr<bool>,
    cx: &Cx,
) -> toasty::Result<ActivityPanel> {
    let activities = Activity::filter(filter)
        .order_by(Activity::fields().created_at().desc())
        // Detail pages show the recent history rather than the whole log; the
        // dashboard's feed is the paginated view of everything logged.
        .limit(50)
        .exec(db)
        .await?;
    let authors = views::author_names(db, &activities).await?;
    Ok(ActivityPanel {
        activities,
        authors,
        zone: views::zone(cx),
    })
}

/// Stamp a record with the user creating it.
///
/// Free-standing so every create handler records authorship the same way, and
/// so the "no user in context" case is decided in one place rather than
/// defaulting differently on each page.
pub fn creator(cx: &Cx) -> Option<i64> {
    auth::current_user(cx).map(|user| user.id)
}

/// Every URL that matches nothing else.
///
/// `not_found!("/")` registers a catch-all page that resolves to a
/// [`topcoat::router::error::NotFoundError`]. Declaring it matters: without a
/// catch-all, a request nothing matched is answered by the router *before* the
/// layouts run, so it would miss the error boundary and the styled 404 page
/// with it. With it, the error bubbles up through [`crate::root`] like any
/// other and comes out looking like the rest of the site.
pub mod not_found {
    topcoat::router::not_found!("/");
}
