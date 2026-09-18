//! Domain vocabulary and small formatting helpers.
//!
//! Money is stored as an integer number of cents and timestamps as Unix
//! seconds, so no floating point and no timezone library are involved.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current time as Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Trim a form value and treat the empty string as absent.
pub fn opt(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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

    /// The value persisted in SQLite.
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
    format!("{}{}.{:02}", if negative { "-" } else { "" }, abs / 100, abs % 100)
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
    let tens = fraction_digits.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
    let ones = fraction_digits.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
    cents = cents.checked_add(tens * 10 + ones)?;
    if fraction_digits.next().is_some_and(|c| c >= '5') {
        cents = cents.checked_add(1)?;
    }

    Some(if negative { -cents } else { cents })
}

// --- Dates -----------------------------------------------------------------

/// Convert a Unix timestamp to a `(year, month, day)` civil date.
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

/// Format a timestamp as `YYYY-MM-DD`.
pub fn format_date(timestamp: i64) -> String {
    let (year, month, day) = civil_from_days(timestamp.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Format a timestamp as `YYYY-MM-DD HH:MM` (UTC).
pub fn format_datetime(timestamp: i64) -> String {
    let days = timestamp.div_euclid(86_400);
    let seconds = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Parse a `YYYY-MM-DD` date into a Unix timestamp at midnight UTC.
pub fn parse_date(input: &str) -> Option<i64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    Some(days_from_civil(year, month, day) * 86_400)
}

/// `YYYY-MM-DD` for a date input's `value` attribute.
pub fn date_input_value(timestamp: Option<i64>) -> String {
    timestamp.map(format_date).unwrap_or_default()
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
            assert_eq!(
                format_date(parsed),
                formatted,
                "round trip for {timestamp}"
            );
        }
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(parse_date("bogus"), None);
        assert_eq!(parse_date(""), None);
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
}
