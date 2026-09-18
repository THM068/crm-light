//! Pagination for the list pages.
//!
//! # What this does and does not solve
//!
//! Every list page loads one page of rows and asks the database for the total
//! separately, so the per-company rollups on `/companies` and the company and
//! contact lookups on `/deals` now only touch the rows on screen. That was the
//! part that grew without bound: a page of 25 rows used to read every contact
//! and every deal in the database.
//!
//! # Why offsets inside an opaque cursor
//!
//! Positions travel as `next=…` / `prev=…` rather than `?page=4`. The value is
//! offset-based underneath, but:
//!
//! - It names the sort column and direction it was minted under, so a link
//!   cannot survive a change to the page's ordering and silently skip or repeat
//!   rows; it falls back to the first page instead.
//! - It is bounded and validated, so `?next=` cannot be used to ask the
//!   database for a million-row scan.
//!
//! `ORDER BY` is always `(sort key, id)`. The id tie-break matters more than it
//! looks: several companies can share a sort key, and without a unique second
//! column the database may return tied rows in a different order for each
//! query, which makes a row appear on two pages or none. It also *breaks* the
//! database's own tie-breaking, which is why the sort column is a plain one:
//! `li.name` would compare under the database's collation while `li.id` is
//! always numeric.
//!
//! The remaining limitation is the usual offset one: a large offset still makes
//! the database walk the rows before it, and an insert landing above the
//! current position shifts the window. Cursor pagination would fix both at the
//! cost of never being able to jump to a page number.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

/// How many rows to read beyond a page, to learn whether a further page exists.
const LOOKAHEAD: usize = 1;

/// The column a list is ordered by, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    /// Name of the sort column, checked against the cursor that names it.
    pub field: &'static str,
    pub direction: Direction,
}

impl Sort {
    pub const fn asc(field: &'static str) -> Self {
        Self {
            field,
            direction: Direction::Asc,
        }
    }

    pub const fn desc(field: &'static str) -> Self {
        Self {
            field,
            direction: Direction::Desc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Asc,
    Desc,
}

/// A validated position in a list.
///
/// Opaque to the browser, and checked against the page's own [`Sort`] before it
/// is used, so a stale or edited link degrades to the first page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// Column the position was minted under.
    pub field: String,
    pub direction: Direction,
    /// Rows to skip.
    pub offset: usize,
}

impl Cursor {
    /// Whether this cursor is usable for a list sorted by `sort`.
    #[must_use]
    pub fn matches(&self, sort: Sort) -> bool {
        self.field == sort.field && self.direction == sort.direction
    }
}

/// Encode a position as an opaque query value.
#[must_use]
pub fn encode(sort: Sort, offset: usize) -> String {
    let direction = match sort.direction {
        Direction::Asc => "a",
        Direction::Desc => "d",
    };
    URL_SAFE_NO_PAD.encode(format!("v1|{}|{direction}|{offset}", sort.field))
}

/// Decode an opaque position, rejecting anything unrecognised.
///
/// A malformed cursor is not an error: a bookmarked link from before a
/// deployment should land on the first page, not on a 400.
#[must_use]
pub fn decode(raw: &str) -> Option<Cursor> {
    let bytes = URL_SAFE_NO_PAD.decode(raw.trim()).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let mut parts = text.split('|');

    if parts.next()? != "v1" {
        return None;
    }
    let field = parts.next()?.to_string();
    let direction = match parts.next()? {
        "a" => Direction::Asc,
        "d" => Direction::Desc,
        _ => return None,
    };
    let offset = parts.next()?.parse().ok()?;
    if parts.next().is_some() || field.is_empty() {
        return None;
    }

    Some(Cursor {
        field,
        direction,
        offset,
    })
}

/// The cursor parameters every list page carries, alongside its own filters.
///
/// Each page flattens this into its `#[query_params]` struct so the query
/// string contract stays visible at the definition site.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CursorParams {
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub prev: Option<String>,
}

/// Resolve the request's cursors into a position and its navigation.
///
/// `next` wins when both are present: a hand-built URL carrying two cursors is
/// ambiguous, and moving forward is the more conservative reading.
#[must_use]
pub fn resolve(params: &CursorParams, sort: Sort) -> Position {
    if let Some(offset) = params
        .next
        .as_deref()
        .and_then(decode)
        .filter(|cursor| cursor.matches(sort))
        .map(|cursor| cursor.offset)
    {
        return Position {
            offset,
            back: false,
        };
    }
    if let Some(offset) = params
        .prev
        .as_deref()
        .and_then(decode)
        .filter(|cursor| cursor.matches(sort))
        .map(|cursor| cursor.offset)
    {
        return Position { offset, back: true };
    }
    Position {
        offset: 0,
        back: false,
    }
}

/// Where a request sits in a list, and how it got there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// Rows to skip.
    pub offset: usize,
    /// Whether the request came from a `prev` link.
    pub back: bool,
}

/// A page of rows, plus the links to move off it.
#[derive(Debug, Clone)]
pub struct Page<T> {
    /// At most `size` rows, in display order.
    pub rows: Vec<T>,
    /// Total rows matching the current filters.
    pub total: usize,
    /// Rows skipped to reach this page.
    pub offset: usize,
    /// Cursor for the following page, when one exists.
    pub next: Option<String>,
    /// Cursor for the preceding page, when one exists.
    pub prev: Option<String>,
}

impl<T> Page<T> {
    /// Whether nothing at all matched.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 1-based index of the first row on this page, for "showing x–y of z".
    #[must_use]
    pub fn showing_from(&self) -> usize {
        if self.rows.is_empty() {
            0
        } else {
            self.offset + 1
        }
    }

    /// 1-based index of the last row on this page.
    #[must_use]
    pub fn showing_to(&self) -> usize {
        self.offset + self.rows.len()
    }
}

/// Assemble a page from the rows a query returned.
///
/// `rows` is what the caller fetched with `LIMIT size + 1 OFFSET offset`; the
/// extra row is what proves another page exists and is dropped here.
#[must_use]
pub fn assemble<T>(
    mut rows: Vec<T>,
    size: usize,
    offset: usize,
    total: usize,
    sort: Sort,
) -> Page<T> {
    let lookahead = rows.len() > size;
    rows.truncate(size);

    Page {
        // One more page exists when the lookahead row came back.
        next: lookahead.then(|| encode(sort, offset + size)),
        // Everything before offset zero is the first page, which has no
        // predecessor, so `prev` is offered only from offset zero onwards.
        prev: (offset > 0).then(|| encode(sort, offset.saturating_sub(size))),
        rows,
        total,
        offset,
    }
}

/// The lookahead the caller should ask the database for.
#[must_use]
pub fn fetch_limit(size: usize) -> usize {
    size + LOOKAHEAD
}

#[cfg(test)]
mod tests {
    use super::*;

    const BY_NAME: Sort = Sort::asc("name");

    fn rows(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    #[test]
    fn cursors_round_trip() {
        for sort in [Sort::asc("name"), Sort::desc("id")] {
            for offset in [0, 1, 25, 1_000_000] {
                let encoded = encode(sort, offset);
                let decoded = decode(&encoded).expect("round trips");
                assert_eq!(decoded.offset, offset);
                assert!(decoded.matches(sort), "{sort:?}");
            }
        }
    }

    #[test]
    fn a_cursor_is_rejected_by_a_list_it_was_not_minted_for() {
        let by_name = decode(&encode(Sort::asc("name"), 50)).expect("valid");
        assert!(by_name.matches(Sort::asc("name")));
        // A different column, or the other direction, invalidates it.
        assert!(!by_name.matches(Sort::desc("name")));
        assert!(!by_name.matches(Sort::asc("created_at")));
    }

    #[test]
    fn malformed_cursors_decode_to_nothing() {
        assert!(decode("").is_none());
        assert!(decode("!!!not base64!!!").is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode("name|a|5")).is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode("v9|name|a|5")).is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode("v1|name|x|5")).is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode("v1|name|a|five")).is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode("v1|name|a|5|extra")).is_none());
        assert!(decode(&URL_SAFE_NO_PAD.encode("v1||a|5")).is_none());
    }

    #[test]
    fn a_stale_cursor_falls_back_to_the_first_page() {
        let params = CursorParams {
            next: Some("garbage".to_string()),
            prev: None,
        };
        let position = resolve(&params, BY_NAME);
        assert_eq!(position.offset, 0);
        assert!(!position.back);
    }

    #[test]
    fn a_cursor_for_another_sort_is_ignored() {
        let params = CursorParams {
            next: Some(encode(Sort::desc("created_at"), 75)),
            prev: None,
        };
        let position = resolve(&params, BY_NAME);
        assert_eq!(position.offset, 0);
    }

    #[test]
    fn next_wins_when_both_cursors_are_present() {
        let params = CursorParams {
            next: Some(encode(BY_NAME, 50)),
            prev: Some(encode(BY_NAME, 25)),
        };
        let position = resolve(&params, BY_NAME);
        assert_eq!(position.offset, 50);
        assert!(!position.back);
    }

    #[test]
    fn prev_marks_the_request_as_going_back() {
        let params = CursorParams {
            next: None,
            prev: Some(encode(BY_NAME, 25)),
        };
        let position = resolve(&params, BY_NAME);
        assert_eq!(position.offset, 25);
        assert!(position.back);
    }

    #[test]
    fn a_full_page_with_a_lookahead_offers_next() {
        // 26 rows fetched for a size of 25.
        let page = assemble(rows(26), 25, 0, 100, BY_NAME);
        assert_eq!(page.rows.len(), 25);
        assert_eq!(page.total, 100);
        assert!(page.prev.is_none(), "the first page has nothing behind it");
        let next = decode(page.next.as_deref().expect("a next cursor")).expect("valid");
        assert_eq!(next.offset, 25);
        assert!(next.matches(BY_NAME));
    }

    #[test]
    fn a_short_page_offers_no_next() {
        let page = assemble(rows(7), 25, 50, 57, BY_NAME);
        assert_eq!(page.rows.len(), 7);
        assert!(page.next.is_none());
        let prev = decode(page.prev.as_deref().expect("a prev cursor")).expect("valid");
        assert_eq!(prev.offset, 25);
    }

    #[test]
    fn the_first_page_of_an_empty_list_has_no_links() {
        let page = assemble(Vec::<usize>::new(), 25, 0, 0, BY_NAME);
        assert!(page.is_empty());
        assert!(page.next.is_none());
        assert!(page.prev.is_none());
    }

    #[test]
    fn an_offset_inside_the_last_page_keeps_prev_reachable() {
        // The final page holds two rows; `prev` still steps a whole page back,
        // because every cursor this app mints lands on a page boundary.
        let page = assemble(rows(2), 25, 50, 52, BY_NAME);
        assert_eq!(page.rows.len(), 2);
        assert!(page.next.is_none());
        let prev = decode(page.prev.as_deref().expect("a prev cursor")).expect("valid");
        assert_eq!(prev.offset, 25);
    }

    #[test]
    fn showing_range_reads_one_based() {
        let page = assemble(rows(25), 25, 25, 100, BY_NAME);
        assert_eq!(page.showing_from(), 26);
        assert_eq!(page.showing_to(), 50);

        let empty = assemble(Vec::<usize>::new(), 25, 0, 0, BY_NAME);
        assert_eq!(empty.showing_from(), 0);
        assert_eq!(empty.showing_to(), 0);
    }

    #[test]
    fn the_fetch_asks_for_one_row_beyond_the_page() {
        assert_eq!(fetch_limit(25), 26);
        assert_eq!(fetch_limit(1), 2);
    }
}
