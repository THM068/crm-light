//! Tenant scoping: who is asking, which workspace they may see, and what
//! happens when they ask for something else.
//!
//! # The two rules
//!
//! 1. **Every read of tenant-owned data filters on `account_id`.** A query
//!    without the filter is the bug this module exists to prevent, so the
//!    filter is built here rather than retyped at each call site.
//! 2. **A row that exists in another workspace answers 403, not 404.** An id
//!    that is not there at all answers 404. The distinction is deliberate: it
//!    tells a signed-in member that they have reached the edge of their
//!    workspace rather than that they mistyped a URL, which is the more useful
//!    answer inside a CRM. It does confirm that *some* row with that id exists
//!    somewhere, so if this ever held data whose mere existence is sensitive,
//!    both branches should collapse into a 404 — and [`Denied::into_error`] is
//!    the one place to change.
//!
//! # Why the check is not in the query
//!
//! [`load`] fetches by id *without* the tenant filter, then decides. Filtering
//! in the query would make a foreign row indistinguishable from an absent one,
//! foreclosing rule 2. The account comparison is what enforces isolation, and
//! it runs on every path: there is no query in this app that reads a
//! tenant-owned row by id without going through here.

use toasty::Db;
use topcoat::router::error::{BadRequestError, ForbiddenError, NotFoundError, bad_request, forbidden, not_found};

use crate::models::{Activity, Company, Contact, Deal, User};

/// A tenant-owned model, loadable under the rules above.
///
/// Implemented for every model carrying an `account_id`. The implementation is
/// mechanical on purpose: a new model cannot be quietly used without the check,
/// because the compiler will not accept it without this trait.
pub trait Loadable: Sized {
    /// Human-readable name, used in the 403 explanation.
    const KIND: &'static str;

    /// The tenant this row belongs to.
    fn account_id(&self) -> i64;

    /// Fetch one row by primary key, *ignoring* tenancy.
    fn fetch(db: &mut Db, id: i64) -> impl Future<Output = toasty::Result<Option<Self>>> + Send;
}

/// Why a row could not be handed to the caller.
///
/// Both variants are real answers rather than failures: a 403 and a 404 are
/// responses, and keeping them apart lets a caller choose to render something
/// for one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    /// The row exists, in another workspace.
    Foreign,
    /// No row with that id exists.
    Absent,
}

impl Denied {
    /// The error for this outcome, ready to bubble out with `?`.
    ///
    /// A `ForbiddenError` carries no description of its own, so the explanation
    /// is attached with `anyhow` context. `topcoat::Error::downcast` searches
    /// the whole chain, so the router still finds the `ForbiddenError`
    /// underneath and answers 403 rather than 500, while the layout's error
    /// boundary can read the context back out to show it.
    #[must_use]
    pub fn into_error(self, kind: &str, id: i64) -> topcoat::Error {
        match self {
            Denied::Foreign => {
                // Wrapping the error type and adding context keeps the
                // `ForbiddenError` as the root cause, which is what the
                // router's downcast needs.
                anyhow::Error::new(forbidden())
                    .context(format!(
                        "That {kind} (id {id}) belongs to a different workspace, so it is not \
                         yours to see or change."
                    ))
                    .into()
            }
            Denied::Absent => not_found().into(),
        }
    }
}

/// The result of a lookup that may have been refused.
pub type Verdict<T> = Result<T, Denied>;

/// Load a row by id on behalf of `account_id`, or say why not.
///
/// # Errors
///
/// Propagates database errors; returns [`Denied`] when the row is absent or
/// belongs to another tenant.
pub async fn load<T: Loadable>(db: &mut Db, account_id: i64, id: i64) -> toasty::Result<Verdict<T>> {
    let Some(row) = T::fetch(db, id).await? else {
        return Ok(Err(Denied::Absent));
    };
    if row.account_id() != account_id {
        return Ok(Err(Denied::Foreign));
    }
    Ok(Ok(row))
}

/// The common handler shape: load, or fail with the right status.
///
/// # Errors
///
/// 403 for a row in another workspace, 404 for a row that is not there, and 500
/// for a database failure.
pub async fn require<T: Loadable>(db: &mut Db, account_id: i64, id: i64) -> topcoat::Result<T> {
    match load::<T>(db, account_id, id).await? {
        Ok(row) => Ok(row),
        Err(denied) => Err(denied.into_error(T::KIND, id)),
    }
}

/// Load an optional reference named by a form.
///
/// A form may name a company, a contact, or a deal. Each must live in the
/// caller's workspace, so a hand-edited `company_id` cannot attach one
/// workspace's record to another's.
///
/// # Errors
///
/// 403 when the referenced row exists elsewhere, 404 when it does not exist.
pub async fn require_reference<T: Loadable>(
    db: &mut Db,
    account_id: i64,
    id: Option<i64>,
) -> topcoat::Result<Option<i64>> {
    match id {
        None => Ok(None),
        Some(id) => {
            require::<T>(db, account_id, id).await?;
            Ok(Some(id))
        }
    }
}

/// Everything a handler needs to know about who is asking.
///
/// Carried in the request context by the guard, so no handler has to remember
/// to re-derive it — and so a handler that forgot to check the tenant still has
/// the value in hand when it builds its query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tenant {
    pub account_id: i64,
    pub user_id: i64,
}

impl Tenant {
    /// The tenant of the signed-in request.
    ///
    /// # Errors
    ///
    /// Returns 401 when the request is anonymous. The guard refuses those
    /// before a handler runs, so this failing means a handler was reached
    /// around the guard.
    pub fn of(cx: &topcoat::context::Cx) -> topcoat::Result<Self> {
        crate::auth::current_user(cx)
            .map(|user| user.tenant())
            .ok_or_else(|| topcoat::router::error::unauthorized().into())
    }
}

// --- Conflict checks -------------------------------------------------------

/// Whether a username is already taken *inside this workspace*.
///
/// Usernames are unique per tenant, not globally, so two workspaces may each
/// have an `admin`. Toasty has no composite unique index for root models, so
/// this check is what enforces it; `username_lower` is indexed to keep it cheap.
///
/// # Errors
///
/// Propagates database errors.
pub async fn username_taken(
    db: &mut Db,
    account_id: i64,
    username_lower: &str,
) -> toasty::Result<bool> {
    let existing = User::filter(
        User::fields()
            .account_id()
            .eq(account_id)
            .and(User::fields().username_lower().eq(username_lower.to_string())),
    )
    .first()
    .exec(db)
    .await?;
    Ok(existing.is_some())
}

/// Validate a username and normalise it, or explain what is wrong.
///
/// Shared by sign-up and account creation so both enforce the same rule.
///
/// # Errors
///
/// Returns a 400 describing the offending character or length.
pub fn check_username(username: &str) -> topcoat::Result<(String, String)> {
    let trimmed = username.trim();
    if trimmed.is_empty() {
        return Err(bad_request("A username is required.").into());
    }
    if trimmed.chars().count() > crate::auth::MAX_USERNAME_LEN {
        return Err(bad_request(format!(
            "Usernames are at most {} characters.",
            crate::auth::MAX_USERNAME_LEN
        ))
        .into());
    }
    if !crate::domain::username_is_well_formed(trimmed) {
        return Err(bad_request(
            "Usernames may use letters, digits, `.`, `_`, `-`, `@`, and `+`.",
        )
        .into());
    }
    Ok((
        trimmed.to_string(),
        crate::domain::normalize_username(trimmed),
    ))
}

/// Validate a workspace name and derive its slug.
///
/// # Errors
///
/// Returns a 400 when the name is empty or produces no usable slug.
pub fn check_account_name(name: &str) -> topcoat::Result<(String, String)> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(bad_request("A workspace name is required.").into());
    }
    if trimmed.chars().count() > 80 {
        return Err(bad_request("Workspace names are at most 80 characters.").into());
    }

    let slug = slugify(trimmed);
    if slug.is_empty() {
        return Err(bad_request(
            "That workspace name has no letters or digits to build a sign-in name from. \
             Add a word to it.",
        )
        .into());
    }
    Ok((trimmed.to_string(), slug))
}

/// Lowercase, hyphenate, and strip anything that is not alphanumeric.
///
/// `Acme Robotics Ltd.` becomes `acme-robotics-ltd`, and `Müller GmbH` becomes
/// `muller-gmbh`: characters are transliterated to ASCII first, so a workspace
/// with an accented name still gets a sign-in slug somebody can type. A script
/// with no ASCII equivalent — Cyrillic, CJK — transliterates to something
/// pronounceable rather than being dropped, and if nothing usable survives at
/// all the caller is told, because the slug is half of the sign-in credential.
///
/// The result is at most [`SLUG_MAX`] characters, and a separator is only ever
/// written once there is a following character to write, so truncation cannot
/// leave a trailing hyphen.
#[must_use]
pub fn slugify(name: &str) -> String {
    let transliterated = deunicode::deunicode(name);
    let mut slug: Vec<char> = Vec::with_capacity(transliterated.len());
    let mut pending_separator = false;

    for ch in transliterated.chars() {
        // Everything that is not a letter or digit is a separator candidate:
        // runs of them collapse into one hyphen, and a trailing run is dropped.
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else {
            pending_separator = true;
            None
        };

        if let Some(next) = next {
            // The hyphen is written here rather than when the separator was
            // seen, so it costs two characters and is skipped if they will not
            // fit.
            if pending_separator && !slug.is_empty() && slug.len() + 2 <= SLUG_MAX {
                slug.push('-');
            }
            pending_separator = false;
            if slug.len() == SLUG_MAX {
                break;
            }
            slug.push(next);
        }
    }

    slug.into_iter().collect()
}

/// Longest slug the app will mint.
pub const SLUG_MAX: usize = 48;

// --- Implementations -------------------------------------------------------
//
// Each is the same three lines, written by hand rather than derived: a derive
// would move the tenant column's name into an attribute, and this is the one
// thing in the codebase that must stay greppable.

macro_rules! impl_loadable {
    ($model:ident, $kind:literal) => {
        impl Loadable for $model {
            const KIND: &'static str = $kind;

            fn account_id(&self) -> i64 {
                self.account_id
            }

            fn fetch(db: &mut Db, id: i64) -> impl Future<Output = toasty::Result<Option<Self>>> + Send {
                let query = $model::filter($model::fields().id().eq(id)).first();
                async move { query.exec(db).await }
            }
        }
    };
}

impl_loadable!(Company, "company");
impl_loadable!(Contact, "contact");
impl_loadable!(Deal, "deal");
impl_loadable!(Activity, "activity");
// A user is tenant-owned too, which is what stops one workspace's
// administrator from managing another's people.
impl_loadable!(User, "account");

/// The error types the layout's error boundary renders.
///
/// Kept together so the boundary and this module cannot drift apart.
#[must_use]
pub fn is_forbidden(error: &topcoat::Error) -> bool {
    error.downcast_ref::<ForbiddenError>().is_some()
}

/// The explanation attached to a 403, when one was attached.
#[must_use]
pub fn forbidden_message(error: &topcoat::Error) -> Option<String> {
    // The context is the reason chain, minus the generic "forbidden" line the
    // error itself contributes.
    let mut reasons: Vec<String> = error
        .chain()
        .map(std::string::ToString::to_string)
        .filter(|reason| reason != "forbidden")
        .collect();
    if reasons.is_empty() {
        return None;
    }
    Some(reasons.swap_remove(0))
}

/// Whether the boundary should render the not-found page.
#[must_use]
pub fn is_not_found(error: &topcoat::Error) -> bool {
    error.downcast_ref::<NotFoundError>().is_some()
}

/// Whether the boundary should render the bad-request page.
#[must_use]
pub fn bad_request_message(error: &topcoat::Error) -> Option<String> {
    error
        .downcast_ref::<BadRequestError>()
        .map(std::string::ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_foreign_row_is_forbidden_and_an_absent_one_is_not_found() {
        let foreign = Denied::Foreign.into_error("company", 7);
        assert!(is_forbidden(&foreign));
        assert!(!is_not_found(&foreign));
        assert!(
            forbidden_message(&foreign)
                .is_some_and(|message| message.contains("different workspace")),
            "the refusal should explain itself: {:?}",
            forbidden_message(&foreign)
        );

        let absent = Denied::Absent.into_error("company", 7);
        assert!(is_not_found(&absent));
        assert!(!is_forbidden(&absent));
    }

    #[test]
    fn attaching_a_reason_does_not_hide_the_status() {
        // The router answers by downcasting the error. If the context wrapper
        // hid the `ForbiddenError`, a cross-tenant request would be a 500
        // instead of a 403 — the failure mode this test exists to catch.
        let error = Denied::Foreign.into_error("deal", 3);
        assert!(error.downcast_ref::<ForbiddenError>().is_some());
        let error = Denied::Absent.into_error("deal", 3);
        assert!(error.downcast_ref::<NotFoundError>().is_some());
    }

    #[test]
    fn a_plain_forbidden_has_no_message() {
        let plain: topcoat::Error = forbidden().into();
        assert!(is_forbidden(&plain));
        assert!(forbidden_message(&plain).is_none());
    }

    #[test]
    fn every_tenant_owned_model_is_loadable() {
        // The isolation check is only enforced for a model that implements
        // `Loadable`. This asserts the list, so a new tenant-owned table cannot
        // be added to `models.rs` without somebody deciding what its `KIND` is
        // and noticing this test change.
        fn assert_loadable<T: Loadable>() {}
        assert_loadable::<Company>();
        assert_loadable::<Contact>();
        assert_loadable::<Deal>();
        assert_loadable::<Activity>();
        assert_loadable::<User>();

        // The kinds are what the 403 message names, so they have to read like
        // nouns a person would recognise.
        assert_eq!(Company::KIND, "company");
        assert_eq!(Contact::KIND, "contact");
        assert_eq!(Deal::KIND, "deal");
        assert_eq!(Activity::KIND, "activity");
        assert_eq!(User::KIND, "account");
    }

    #[test]
    fn the_refusal_names_the_record_it_refused() {
        // A bare "forbidden" tells somebody nothing about what went wrong. The
        // message has to name the kind and the id, because the most likely
        // reader is a person who mistyped or followed an old link.
        let error = Denied::Foreign.into_error("contact", 42);
        let message = forbidden_message(&error).expect("a reason");
        assert!(message.contains("contact"), "{message}");
        assert!(message.contains("42"), "{message}");
        assert!(message.contains("different workspace"), "{message}");
    }

    #[test]
    fn unrelated_errors_match_nothing() {
        let other: topcoat::Error = bad_request("bad input").into();
        assert!(!is_forbidden(&other));
        assert!(!is_not_found(&other));
        // `Display` on the router's error includes its own status prefix.
        assert_eq!(
            bad_request_message(&other).as_deref(),
            Some("bad request: bad input")
        );
    }

    #[test]
    fn slugs_are_url_safe_and_stable() {
        assert_eq!(slugify("Acme Robotics Ltd."), "acme-robotics-ltd");
        assert_eq!(slugify("  Spaced   Out  "), "spaced-out");
        assert_eq!(slugify("50% Off Supplies"), "50-off-supplies");
        assert_eq!(slugify("___"), "");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
        // Long names are truncated without leaving a trailing hyphen.
        assert_eq!(slugify(&"a".repeat(60)).len(), SLUG_MAX);
        assert_eq!(slugify(&format!("{} b", "a".repeat(SLUG_MAX - 1))).len(), SLUG_MAX);
    }

    #[test]
    fn accented_names_still_produce_a_typable_slug() {
        // The slug is half of the sign-in credential, so a workspace with an
        // accented name must not end up with one nobody can type.
        assert_eq!(slugify("Müller GmbH"), "muller-gmbh");
        assert_eq!(slugify("Café Crème"), "cafe-creme");
        assert_eq!(slugify("Ørsted Energy"), "orsted-energy");
        assert_eq!(slugify("Łódź Textiles"), "lodz-textiles");

        // Scripts with no ASCII form transliterate rather than vanish, so the
        // slug is recognisable instead of a fragment of the name.
        assert_eq!(slugify("Москва"), "moskva");
        assert_eq!(slugify("東京"), "dong-jing");
    }

    #[test]
    fn usernames_are_validated_and_normalised() {
        let (username, lower) = check_username("  Ada.Lovelace  ").expect("valid");
        assert_eq!(username, "Ada.Lovelace");
        assert_eq!(lower, "ada.lovelace");

        assert!(check_username("").is_err());
        assert!(check_username("   ").is_err());
        assert!(check_username("has space").is_err());
        assert!(check_username(&"x".repeat(65)).is_err());
    }

    #[test]
    fn account_names_must_produce_a_usable_slug() {
        let (name, slug) = check_account_name(" Acme Robotics ").expect("valid");
        assert_eq!(name, "Acme Robotics");
        assert_eq!(slug, "acme-robotics");

        assert!(check_account_name("").is_err());
        assert!(check_account_name("!!!" ).is_err());
        assert!(check_account_name(&"x".repeat(81)).is_err());
    }
}
