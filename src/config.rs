//! Process configuration, read once at startup from the environment.
//!
//! Nothing else in the app reads `std::env`: every knob is parsed and validated
//! here, so a misconfigured deployment fails loudly at boot rather than at the
//! first request.

use std::time::Duration;

use jiff::tz::TimeZone as JiffTimeZone;
use topcoat::cookie::Key;

use crate::domain::TimeZone;

/// Where the app connects and how it behaves.
#[derive(Debug, Clone)]
pub struct Config {
    /// Toasty connection URL. `postgresql://…` is the supported backend;
    /// `sqlite:…` remains available for tests and throwaway runs.
    pub database_url: String,

    /// Zone used to render every timestamp shown to a user.
    pub time_zone: TimeZone,

    /// Key siging and encrypting cookies. Derived from `CRM_SECRET_KEY`, or
    /// generated for the process when that is unset.
    pub cookie_key: Key,

    /// Whether `Secure` is set on session and CSRF cookies. Off by default so
    /// plain-HTTP local development works; turn it on behind TLS.
    pub cookie_secure: bool,

    /// How long an idle session stays valid.
    pub session_ttl: Duration,

    /// Rows per list page.
    pub page_size: usize,

    /// Failed logins against one username (from anywhere) before that account
    /// is temporarily locked.
    pub login_max_attempts: i64,

    /// How long the lock lasts.
    pub login_lockout: Duration,

    /// Whether a user with no password set may log in by leaving the password
    /// field blank.
    pub allow_passwordless_login: bool,

    /// Insert the demo dataset when the database has no companies.
    pub seed_demo_data: bool,
}

/// Why the configuration could not be used.
#[derive(Debug)]
pub struct ConfigError(String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ConfigError {}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_i64(name: &str, default: i64) -> Result<i64, ConfigError> {
    match env(name) {
        None => Ok(default),
        Some(raw) => raw
            .trim()
            .parse()
            .map_err(|_| ConfigError(format!("{name} must be an integer, got {raw:?}"))),
    }
}

fn env_bool(name: &str, default: bool) -> Result<bool, ConfigError> {
    match env(name) {
        None => Ok(default),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => Err(ConfigError(format!(
                "{name} must be a boolean (1/0, true/false), got {other:?}"
            ))),
        },
    }
}

/// Non-fatal notes about the resolved configuration, logged at startup.
#[derive(Debug, Clone, Default)]
pub struct ConfigWarnings(pub Vec<String>);

impl Config {
    /// The documented default connection string.
    pub const DEFAULT_DATABASE_URL: &'static str = "postgresql://localhost/crm_light";

    /// Read and validate the configuration from the environment.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] naming the offending variable when a value is
    /// present but unusable.
    pub fn from_env() -> Result<(Self, ConfigWarnings), ConfigError> {
        let mut warnings = Vec::new();

        let database_url =
            env("CRM_DB").unwrap_or_else(|| Self::DEFAULT_DATABASE_URL.to_string());

        // An explicit CRM_TZ must resolve: a typo should not silently fall back
        // to the host zone and render every timestamp in the wrong place.
        let time_zone = match env("CRM_TZ") {
            Some(name) => TimeZone::parse(&name)
                .ok_or_else(|| ConfigError(format!("CRM_TZ {name:?} is not a known IANA zone")))?,
            None => match JiffTimeZone::try_system() {
                Ok(zone) => TimeZone::parse(zone.iana_name().unwrap_or("UTC"))
                    .unwrap_or_else(TimeZone::utc),
                Err(_) => TimeZone::utc(),
            },
        };

        let cookie_key = match env("CRM_SECRET_KEY") {
            Some(secret) => {
                let bytes = secret.as_bytes();
                if bytes.len() < 64 {
                    return Err(ConfigError(format!(
                        "CRM_SECRET_KEY must be at least 64 bytes (got {}); \
                         generate one with `openssl rand -base64 64`",
                        bytes.len()
                    )));
                }
                // Keep the newest 64 bytes: the cookie crate derives its
                // signing and encryption keys from the tail of the slice.
                Key::from(&bytes[bytes.len() - 64..])
            }
            None => {
                warnings.push(
                    "CRM_SECRET_KEY is unset, so a random cookie key was generated for this \
                     process; every restart invalidates all sessions. Set it in any \
                     deployment that outlives one request."
                        .to_string(),
                );
                Key::generate()
            }
        };

        let session_ttl_hours = env_i64("CRM_SESSION_TTL_HOURS", 24 * 14)?;
        if session_ttl_hours < 1 {
            return Err(ConfigError(
                "CRM_SESSION_TTL_HOURS must be at least 1".to_string(),
            ));
        }

        let page_size = env_i64("CRM_PAGE_SIZE", 25)?;
        if !(1..=500).contains(&page_size) {
            return Err(ConfigError(
                "CRM_PAGE_SIZE must be between 1 and 500".to_string(),
            ));
        }

        let login_max_attempts = env_i64("CRM_LOGIN_MAX_ATTEMPTS", 8)?;
        if login_max_attempts < 1 {
            return Err(ConfigError(
                "CRM_LOGIN_MAX_ATTEMPTS must be at least 1".to_string(),
            ));
        }

        let lockout_minutes = env_i64("CRM_LOGIN_LOCKOUT_MINUTES", 15)?;
        if lockout_minutes < 1 {
            return Err(ConfigError(
                "CRM_LOGIN_LOCKOUT_MINUTES must be at least 1".to_string(),
            ));
        }

        let allow_passwordless_login = env_bool("CRM_ALLOW_EMPTY_PASSWORD", true)?;
        if allow_passwordless_login {
            warnings.push(
                "CRM_ALLOW_EMPTY_PASSWORD is on: an account with no stored password (the \
                 bootstrap admin, until you set one) can be signed into with a blank password. \
                 Set a password on every account, or set CRM_ALLOW_EMPTY_PASSWORD=0, before \
                 exposing this app."
                    .to_string(),
            );
        }

        let config = Self {
            database_url,
            time_zone,
            cookie_key,
            cookie_secure: env_bool("CRM_COOKIE_SECURE", false)?,
            session_ttl: Duration::from_secs(session_ttl_hours as u64 * 3600),
            page_size: page_size as usize,
            login_max_attempts,
            login_lockout: Duration::from_secs(lockout_minutes as u64 * 60),
            allow_passwordless_login,
            // `CRM_SEED=0` disables; anything else (including unset) seeds.
            seed_demo_data: env("CRM_SEED").as_deref() != Some("0"),
        };

        Ok((config, ConfigWarnings(warnings)))
    }

    /// Whether the connection URL selects a backend this app can actually run
    /// on.
    ///
    /// The dashboard's aggregates are written in PostgreSQL SQL and the search
    /// asks for `ILIKE`, which only PostgreSQL has, so a SQLite URL would boot
    /// and then fail on the first page that used either. Refusing at startup
    /// turns that into one clear message instead of a 500 per request. The
    /// SQLite driver stays compiled in for the migration CLI, which only needs
    /// to generate and apply DDL.
    #[must_use]
    pub fn supports(&self) -> bool {
        let scheme = self
            .database_url
            .split("://")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // SQLite URLs in Toasty are written `sqlite:path` with no `//`.
        if self.database_url.to_ascii_lowercase().starts_with("sqlite:") {
            return false;
        }
        matches!(scheme.as_str(), "postgresql" | "postgres")
    }

    /// Which driver the connection URL selects, for the startup banner.
    pub fn driver_name(&self) -> String {
        let scheme = self
            .database_url
            .split([':', '/'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        match scheme.as_str() {
            "postgresql" | "postgres" => "PostgreSQL".to_string(),
            "sqlite" => "SQLite".to_string(),
            other => other.to_string(),
        }
    }

    #[cfg(test)]
    pub fn for_tests(database_url: &str) -> Self {
        Self {
            database_url: database_url.to_string(),
            time_zone: TimeZone::utc(),
            cookie_key: Key::generate(),
            cookie_secure: false,
            session_ttl: Duration::from_secs(3600),
            page_size: 5,
            login_max_attempts: 3,
            login_lockout: Duration::from_secs(60),
            allow_passwordless_login: true,
            seed_demo_data: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_name_follows_the_url_scheme() {
        let mut config = Config::for_tests("postgresql://localhost/crm_light");
        assert_eq!(config.driver_name(), "PostgreSQL");
        assert!(config.supports());
        config.database_url = "postgres://user@host/db".to_string();
        assert!(config.supports());
        config.database_url = "sqlite:crm.db".to_string();
        assert_eq!(config.driver_name(), "SQLite");
        assert!(!config.supports());
        assert!(!Config::for_tests("sqlite::memory:").supports());
        assert!(!Config::for_tests("mysql://host/db").supports());
    }
}
