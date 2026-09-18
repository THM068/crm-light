//! Text search across the list pages.
//!
//! # What `format!("%{term}%")` got wrong
//!
//! Passing the user's text straight into the pattern made `%` and `_` live
//! wildcards: a search for `50%` matched every row, and `a_b` matched `axb`.
//! [`like_contains`] escapes them and the query carries an explicit `ESCAPE`
//! clause, so a term only ever matches itself.
//!
//! # Case sensitivity
//!
//! `LIKE` is case-insensitive for ASCII on SQLite but **case-sensitive on
//! PostgreSQL**, which is why the search that used to work stopped working the
//! moment the app moved. PostgreSQL has `ILIKE` for exactly this, and Toasty
//! exposes it as `.ilike()`, so the app asks for case-insensitive matching
//! explicitly rather than relying on whichever backend it happens to be on.
//!
//! `.ilike()` is rejected by the SQLite driver and this app refuses to run on
//! SQLite anyway, so the escaping is what the unit tests below can check
//! without a server; the operator itself is verified against PostgreSQL.
//! `LIKE`/`ILIKE ... ESCAPE '\'` is supported by both, which is why `\` is the
//! escape character that [`like_contains`] builds for.

use toasty::stmt::Expr;
use toasty::stmt::Path;

use crate::domain::{LIKE_ESCAPE, like_contains};

/// A string-valued field that can be matched case-insensitively.
pub trait CaseInsensitiveLike {
    /// `field ILIKE '%term%'`, with the term's own wildcards escaped.
    fn contains_ignoring_case(self, term: &str) -> Expr<bool>;
}

impl<T> CaseInsensitiveLike for Path<T, String> {
    fn contains_ignoring_case(self, term: &str) -> Expr<bool> {
        self.ilike_with_escape(like_contains(term), LIKE_ESCAPE)
    }
}

impl<T> CaseInsensitiveLike for Path<T, Option<String>> {
    fn contains_ignoring_case(self, term: &str) -> Expr<bool> {
        self.ilike_with_escape(like_contains(term), LIKE_ESCAPE)
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::like_contains;

    #[test]
    fn wildcards_in_a_term_become_literal_characters() {
        // The pattern the query will carry: every wildcard escaped, wrapped in
        // `%…%` so the term can appear anywhere in the column.
        assert_eq!(like_contains("50%"), "%50\\%%");
        assert_eq!(like_contains("a_b"), "%a\\_b%");
        assert_eq!(like_contains("100%_done"), "%100\\%\\_done%");
    }

    #[test]
    fn the_escape_character_itself_is_escaped() {
        assert_eq!(like_contains("back\\slash"), "%back\\\\slash%");
    }

    #[test]
    fn an_empty_term_matches_everything() {
        // Callers are expected to skip the filter entirely for an empty term;
        // this only documents what the pattern would be if they did not.
        assert_eq!(like_contains(""), "%%");
    }
}
