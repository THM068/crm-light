//! Domain vocabulary and small formatting helpers.
//!
//! Money is stored as an integer number of cents and every timestamp as Unix
//! seconds, so the schema never depends on a database date type and there is no
//! floating point anywhere in the money path.
//!
//! Timestamps are *stored* in UTC. Every value shown to a user is rendered
//! through a [`TimeZone`], resolved once at startup from `CRM_TZ` (falling back
//! to the host's zone), so the same row reads correctly wherever the app runs
//! and a DST transition never shifts a displayed time by an hour.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::{Timestamp, tz::TimeZone as JiffTimeZone};

/// Current time as Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Seconds in a day.
pub const DAY: i64 = 86_400;

/// Trim a form value and treat the empty string as absent.
pub fn opt(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// --- Time zone -------------------------------------------------------------

/// The display time zone, wrapping the IANA database.
///
/// Cheap to clone: `jiff` shares the underlying zone data behind an `Arc`.
#[derive(Debug, Clone)]
pub struct TimeZone(JiffTimeZone);

impl TimeZone {
    /// UTC, used in tests and as the fallback when a zone cannot be resolved.
    pub fn utc() -> Self {
        Self(JiffTimeZone::UTC)
    }

    /// The host's zone, or UTC when it cannot be determined.
    pub fn system() -> Self {
        JiffTimeZone::try_system()
            .map(Self)
            .unwrap_or_else(|_| Self::utc())
    }

    /// Resolve an IANA name such as `Europe/London` or `America/New_York`.
    ///
    /// A leading `UTC`/`utc` is accepted explicitly, since `jiff`'s `get` wants
    /// a real IANA name.
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if name.eq_ignore_ascii_case("utc") || name.eq_ignore_ascii_case("z") {
            return Some(Self::utc());
        }

        JiffTimeZone::get(name).ok().map(Self)
    }

    /// The zone's IANA name, e.g. `Some("Europe/London")`.
    pub fn name(&self) -> Option<&str> {
        self.0.iana_name()
    }

    /// The UTC offset in effect at `timestamp`, in seconds.
    pub fn offset_seconds(&self, timestamp: i64) -> Option<i32> {
        Timestamp::from_second(timestamp)
            .ok()
            .map(|ts| self.0.to_offset(ts).seconds())
    }

    /// `YYYY-MM-DD HH:MM` in this zone.
    pub fn format_datetime(&self, timestamp: i64) -> String {
        match Timestamp::from_second(timestamp) {
            Ok(ts) => ts.to_zoned(self.0.clone()).strftime("%Y-%m-%d %H:%M").to_string(),
            Err(_) => "—".to_string(),
        }
    }

    /// `YYYY-MM-DD` in this zone.
    pub fn format_date(&self, timestamp: i64) -> String {
        match Timestamp::from_second(timestamp) {
            Ok(ts) => ts.to_zoned(self.0.clone()).strftime("%Y-%m-%d").to_string(),
            Err(_) => "—".to_string(),
        }
    }

    /// Midnight at the start of the civil date containing `timestamp`.
    ///
    /// Dates are stored as the instant of local midnight so that a date shown
    /// back through this zone is the date the user typed, even across a DST
    /// boundary.
    pub fn start_of_day(&self, timestamp: i64) -> Option<i64> {
        let ts = Timestamp::from_second(timestamp).ok()?;
        let date = ts.to_zoned(self.0.clone()).date();
        date.at(0, 0, 0, 0)
            .to_zoned(self.0.clone())
            .ok()
            .map(|zdt| zdt.timestamp().as_second())
    }
}

impl fmt::Display for TimeZone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name().unwrap_or("UTC"))
    }
}

/// `YYYY-MM-DD HH:MM` for a timestamp, in UTC.
///
/// Prefer [`TimeZone::format_datetime`]; this exists for tests and for the
/// migration-time defaults where no zone is in hand.
pub fn format_datetime(timestamp: i64) -> String {
    TimeZone::utc().format_datetime(timestamp)
}

/// `YYYY-MM-DD` for a timestamp, in UTC.
pub fn format_date(timestamp: i64) -> String {
    TimeZone::utc().format_date(timestamp)
}

/// `YYYY-MM-DD` for a date input's `value` attribute, in UTC.
pub fn date_input_value(timestamp: Option<i64>) -> String {
    timestamp.map(format_date).unwrap_or_default()
}

// --- Deal stage ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Lead,
    Qualified,
    Proposal,
    Negotiation,
    Won,
    Lost,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Lead,
        Stage::Qualified,
        Stage::Proposal,
        Stage::Negotiation,
        Stage::Won,
        Stage::Lost,
    ];

    /// The value persisted in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Lead => "lead",
            Stage::Qualified => "qualified",
            Stage::Proposal => "proposal",
            Stage::Negotiation => "negotiation",
            Stage::Won => "won",
            Stage::Lost => "lost",
        }
    }

    /// Human-readable name.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Lead => "Lead",
            Stage::Qualified => "Qualified",
            Stage::Proposal => "Proposal",
            Stage::Negotiation => "Negotiation",
            Stage::Won => "Won",
            Stage::Lost => "Lost",
        }
    }

    pub fn parse(value: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// Round-trip a stored string, falling back to `Lead` for unknown values.
    pub fn from_stored(value: &str) -> Stage {
        Stage::parse(value).unwrap_or(Stage::Lead)
    }

    /// Open stages make up the live pipeline.
    pub fn is_open(self) -> bool {
        !matches!(self, Stage::Won | Stage::Lost)
    }

    /// CSS class used to colour the stage badge.
    pub fn css_class(self) -> &'static str {
        match self {
            Stage::Lead => "stage-lead",
            Stage::Qualified => "stage-qualified",
            Stage::Proposal => "stage-proposal",
            Stage::Negotiation => "stage-negotiation",
            Stage::Won => "stage-won",
            Stage::Lost => "stage-lost",
        }
    }
}

// --- User role -------------------------------------------------------------

/// What an account is allowed to do.
///
/// Every signed-in user may read and write the CRM records. The role gates the
/// administration surface: managing accounts, and changing anyone's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Full access, including `/admin/users`.
    Admin,
    /// Ordinary access to the CRM data, but not to account management.
    Member,
}

impl Role {
    pub const ALL: [Role; 2] = [Role::Member, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Admin => "Administrator",
            Role::Member => "Member",
        }
    }

    /// What the role may do, shown next to it in the user list.
    pub fn description(self) -> &'static str {
        match self {
            Role::Admin => "Full access, including accounts",
            Role::Member => "Read and write CRM records",
        }
    }

    pub fn parse(value: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == value)
    }

    /// Round-trip a stored string, defaulting to the least privileged role.
    ///
    /// An unrecognised value means the row was written by something that does
    /// not know about the current role set, so it must not grant admin.
    pub fn from_stored(value: &str) -> Role {
        Role::parse(value).unwrap_or(Role::Member)
    }

    pub fn is_admin(self) -> bool {
        matches!(self, Role::Admin)
    }
}

/// Normalise a username for storage and lookup.
///
/// Live usernames keep their original case; this is what the unique index
/// covers, so adding `Ada` when `ada` exists is a conflict rather than a second
/// account that only fails at sign-in time.
pub fn normalize_username(username: &str) -> String {
    username.trim().to_ascii_lowercase()
}

/// Characters allowed in a username, besides being non-empty and at most 64
/// characters.
pub fn username_is_well_formed(username: &str) -> bool {
    let trimmed = username.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 64
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | '+'))
}

// --- Activity kind ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Note,
    Call,
    Email,
    Meeting,
}

impl ActivityKind {
    pub const ALL: [ActivityKind; 4] = [
        ActivityKind::Note,
        ActivityKind::Call,
        ActivityKind::Email,
        ActivityKind::Meeting,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ActivityKind::Note => "note",
            ActivityKind::Call => "call",
            ActivityKind::Email => "email",
            ActivityKind::Meeting => "meeting",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::Note => "Note",
            ActivityKind::Call => "Call",
            ActivityKind::Email => "Email",
            ActivityKind::Meeting => "Meeting",
        }
    }

    pub fn from_stored(value: &str) -> ActivityKind {
        ActivityKind::ALL
            .into_iter()
            .find(|k| k.as_str() == value)
            .unwrap_or(ActivityKind::Note)
    }
}

// --- Money -----------------------------------------------------------------

/// Format an integer number of cents as `$1,234.56`.
pub fn format_money(cents: i64) -> String {
    let negative = cents < 0;
    let abs = cents.unsigned_abs();
    let dollars = abs / 100;
    let remainder = abs % 100;

    let digits = dollars.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        // Insert a separator every three digits, counting from the left.
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }

    format!(
        "{}{}.{:02}",
        if negative { "-$" } else { "$" },
        grouped,
        remainder
    )
}

/// Plain decimal (no symbol or grouping) for a money `<input value=...>`.
pub fn money_input_value(cents: i64) -> String {
    let negative = cents < 0;
    let abs = cents.unsigned_abs();
    format!(
        "{}{}.{:02}",
        if negative { "-" } else { "" },
        abs / 100,
        abs % 100
    )
}

/// Parse user input such as `1234.56`, `$1,234.56`, or `1234` into cents.
pub fn parse_money(input: &str) -> Option<i64> {
    let cleaned: String = input
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | ' ' | '_'))
        .collect();
    if cleaned.is_empty() {
        return None;
    }

    let (negative, digits) = match cleaned.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, cleaned.as_str()),
    };

    let (whole, fraction) = match digits.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (digits, ""),
    };

    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    if !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Round to the nearest cent rather than truncating.
    let mut cents = whole.checked_mul(100)?;
    let mut fraction_digits = fraction.chars();
    let tens = fraction_digits
        .next()
        .and_then(|c| c.to_digit(10))
        .unwrap_or(0) as i64;
    let ones = fraction_digits
        .next()
        .and_then(|c| c.to_digit(10))
        .unwrap_or(0) as i64;
    cents = cents.checked_add(tens * 10 + ones)?;
    if fraction_digits.next().is_some_and(|c| c >= '5') {
        cents = cents.checked_add(1)?;
    }

    Some(if negative { -cents } else { cents })
}

// --- Dates -----------------------------------------------------------------

/// Convert a Unix timestamp to a `(year, month, day)` civil date, in UTC.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant's `civil_from_days`, shifting the epoch to 0000-03-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Convert a civil date to days since the Unix epoch.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Parse a `YYYY-MM-DD` date, rejecting impossible calendar dates.
///
/// Returns the civil date; the caller decides what instant to store it as, via
/// [`TimeZone::start_of_day`] or [`parse_date`].
pub fn parse_civil_date(input: &str) -> Option<(i64, u32, u32)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 {
        return None;
    }

    // Round-trip the day through the civil conversion to reject 2026-02-30 and
    // friends, which would otherwise roll over into March.
    if civil_from_days(days_from_civil(year, month, day)) != (year, month, day) {
        return None;
    }

    Some((year, month, day))
}

/// Parse a `YYYY-MM-DD` date into a Unix timestamp at midnight UTC.
pub fn parse_date(input: &str) -> Option<i64> {
    let (year, month, day) = parse_civil_date(input)?;
    Some(days_from_civil(year, month, day) * DAY)
}

/// Parse a `YYYY-MM-DD` date into the instant of midnight in `zone`.
///
/// This is what the deal form uses, so an "expected close" of 2026-09-18 is
/// stored as the start of that day locally and displays back as 2026-09-18.
pub fn parse_date_in(input: &str, zone: &TimeZone) -> Option<i64> {
    let (year, month, day) = parse_civil_date(input)?;
    let utc_midnight = days_from_civil(year, month, day) * DAY;
    zone.start_of_day(utc_midnight).or(Some(utc_midnight))
}

/// "Ada Lovelace" from the two name parts.
pub fn full_name(first: &str, last: &str) -> String {
    match (first.trim().is_empty(), last.trim().is_empty()) {
        (false, false) => format!("{} {}", first.trim(), last.trim()),
        (false, true) => first.trim().to_string(),
        (true, false) => last.trim().to_string(),
        (true, true) => "(unnamed)".to_string(),
    }
}

// --- Text search -----------------------------------------------------------

/// Escape the `LIKE` metacharacters in a user-supplied search term.
///
/// Without this a search for `50%` matches every company, and `a_b` matches
/// `axb`. The pattern is built for an `ESCAPE '\'` clause, so `\`, `%` and `_`
/// are all escaped and the term only ever matches literally.
pub fn like_contains(term: &str) -> String {
    let mut pattern = String::with_capacity(term.len() + 2);
    pattern.push('%');
    for ch in term.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

/// The `LIKE` escape character [`like_contains`] builds patterns for.
pub const LIKE_ESCAPE: char = '\\';

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_round_trips() {
        assert_eq!(format_money(0), "$0.00");
        assert_eq!(format_money(5), "$0.05");
        assert_eq!(format_money(123_456), "$1,234.56");
        assert_eq!(format_money(100_000_000), "$1,000,000.00");
        assert_eq!(format_money(-2_550), "-$25.50");
    }

    #[test]
    fn money_parses_user_input() {
        assert_eq!(parse_money("1234.56"), Some(123_456));
        assert_eq!(parse_money("$1,234.56"), Some(123_456));
        assert_eq!(parse_money("1234"), Some(123_400));
        assert_eq!(parse_money(""), None);
        assert_eq!(parse_money("abc"), None);
        // A third decimal place rounds rather than truncating.
        assert_eq!(parse_money("1.005"), Some(101));
    }

    #[test]
    fn dates_round_trip_across_epochs() {
        for timestamp in [0, 1_700_000_000, 2_000_000_000, 951_782_400] {
            let formatted = format_date(timestamp);
            let parsed = parse_date(&formatted).expect("formatted date parses");
            assert_eq!(format_date(parsed), formatted, "round trip for {timestamp}");
        }
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("bogus"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn dates_reject_impossible_calendar_days() {
        assert_eq!(parse_date("2026-02-30"), None);
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("2026-04-31"), None);
        assert_eq!(parse_date("2026-00-10"), None);
        // 2024 is a leap year, 2026 is not.
        assert!(parse_date("2024-02-29").is_some());
        assert_eq!(parse_date("2026-02-29"), None);
    }

    #[test]
    fn stage_round_trips() {
        for stage in Stage::ALL {
            assert_eq!(Stage::parse(stage.as_str()), Some(stage));
        }
        assert_eq!(Stage::from_stored("nonsense"), Stage::Lead);
        assert!(!Stage::Won.is_open());
        assert!(Stage::Lead.is_open());
    }

    #[test]
    fn search_terms_are_escaped() {
        assert_eq!(like_contains("acme"), "%acme%");
        assert_eq!(like_contains("50%"), "%50\\%%");
        assert_eq!(like_contains("a_b"), "%a\\_b%");
        assert_eq!(like_contains("c:\\x"), "%c:\\\\x%");
    }

    #[test]
    fn utc_zone_is_the_identity() {
        let utc = TimeZone::utc();
        assert_eq!(utc.format_datetime(0), "1970-01-01 00:00");
        assert_eq!(utc.format_date(1_700_000_000), "2023-11-14");
        assert_eq!(utc.offset_seconds(0), Some(0));
    }

    #[test]
    fn zones_shift_the_rendered_hour() {
        // 2023-11-14T22:13:20Z. London is on GMT in November, New York is
        // five hours behind.
        let london = TimeZone::parse("Europe/London").expect("known zone");
        let new_york = TimeZone::parse("America/New_York").expect("known zone");
        assert_eq!(london.format_datetime(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(new_york.format_datetime(1_700_000_000), "2023-11-14 17:13");

        // Auckland's +13 in January pushes a late-UTC timestamp into the next
        // civil day.
        let auckland = TimeZone::parse("Pacific/Auckland").expect("known zone");
        assert_eq!(auckland.format_date(1_700_000_000), "2023-11-15");
    }

    #[test]
    fn zone_parsing_rejects_nonsense() {
        assert!(TimeZone::parse("Mars/Olympus").is_none());
        assert!(TimeZone::parse("").is_none());
        assert!(TimeZone::parse("utc").is_some());
    }

    #[test]
    fn local_midnight_round_trips_through_a_dst_transition() {
        // British Summer Time ends on 2024-10-27; the day is 25 hours long.
        let london = TimeZone::parse("Europe/London").expect("known zone");
        let ts = parse_date_in("2024-10-27", &london).expect("valid date");
        assert_eq!(london.format_date(ts), "2024-10-27");

        // On that date midnight is still BST (+1), not GMT.
        assert_eq!(london.offset_seconds(ts), Some(3600));

        // And the spring transition, where midnight is GMT (+0).
        let ts = parse_date_in("2024-03-31", &london).expect("valid date");
        assert_eq!(london.format_date(ts), "2024-03-31");
        assert_eq!(london.offset_seconds(ts), Some(0));
    }
}
