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

    /// Whether a stranger may create a workspace at `/signup`.
    ///
    /// On by default so a fresh installation is reachable. Turn it off once the
    /// workspaces you want exist: on a server that anything can reach, an open
    /// sign-up lets anyone who finds the port create a tenant, and the app has
    /// no invitation, email check, or CAPTCHA to slow that down.
    pub allow_signup: bool,

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

/// Replace the password in a connection URL with `***`, for a message that may
/// be read over somebody's shoulder or pasted into a chat.
#[must_use]
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    match rest.split_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            // A password is present: keep the user, hide the secret. Printing
            // `***` for a URL that has no password at all would be worse than
            // useless in a diagnostic about a *missing* password, because it
            // would suggest one is there.
            Some((user, _password)) => format!("{scheme}://{user}:***@{host}"),
            None => format!("{scheme}://{userinfo}@{host}"),
        },
        None => url.to_string(),
    }
}

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
                "CRM_ALLOW_EMPTY_PASSWORD is on: an account with no stored password can be \
                 signed into with a blank password. Nothing creates such an account any more \
                 — sign-up always sets one, and so does an administrator — so this only \
                 affects rows carried over from an older version. On a server, set it to 0."
                    .to_string(),
            );
        }

        let allow_signup = env_bool("CRM_ALLOW_SIGNUP", true)?;
        if allow_signup {
            warnings.push(
                "CRM_ALLOW_SIGNUP is on: anyone who can reach this port can create a workspace. \
                 Set CRM_ALLOW_SIGNUP=0 on a server that is reachable from outside, once the \
                 workspaces you want exist."
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
            allow_signup,
            // `CRM_SEED=0` disables; anything else (including unset) seeds.
            seed_demo_data: env("CRM_SEED").as_deref() != Some("0"),
        };

        Ok((config, ConfigWarnings(warnings)))
    }

    /// Whether the connection URL carries a password, and why not if it does
    /// not.
    ///
    /// The Postgres driver reports a URL without a password as
    /// "invalid configuration: password missing", which says nothing about
    /// where the password should have come from. Checking here turns that into
    /// a message that names the variable and shows the URL's shape.
    ///
    /// The compiled-in default has no password on purpose: it is a
    /// development default that works over a unix socket with peer
    /// authentication, and it is exactly the value the app falls back to when
    /// `CRM_DB` is not in the environment — which is the usual reason a
    /// deployment sees this at all.
    ///
    /// # Errors
    ///
    /// Returns a message naming what is missing and how to supply it.
    pub fn check_database_url(&self) -> Result<(), ConfigError> {
        let url = &self.database_url;

        // Passwords and user names are percent-encoded in the userinfo section;
        // an `@` inside a password would therefore be escaped, so the first `@`
        // is the separator.
        let Some((_scheme, rest)) = url.split_once("://") else {
            return Ok(()); // Not a URL shape this check understands; `supports` will reject it.
        };
        let Some((userinfo, _host)) = rest.split_once('@') else {
            return Err(ConfigError(self.missing_password_message(
                "there is no user name or password in it",
            )));
        };
        let Some((user, password)) = userinfo.split_once(':') else {
            return Err(ConfigError(self.missing_password_message(
                "it has a user name but no password",
            )));
        };
        if password.is_empty() {
            return Err(ConfigError(
                self.missing_password_message("the password is empty"),
            ));
        }
        if user.is_empty() {
            return Err(ConfigError(
                self.missing_password_message("the user name is empty"),
            ));
        }
        Ok(())
    }

    /// The explanation for a URL that cannot authenticate.
    fn missing_password_message(&self, problem: &str) -> String {
        let from_default = self.database_url == Self::DEFAULT_DATABASE_URL;
        let mut message = format!(
            "CRM_DB is not usable: {problem}.\n  \n  \
             CRM_DB is currently: {}\n  \n  \
             A working value puts the user name and password before the host, \
             like this:\n    \
             postgresql://USER:PASSWORD@127.0.0.1:5432/crm_light?sslmode=disable",
            redact_url(&self.database_url)
        );
        if from_default {
            message.push_str(
                "\n  \n  That is the compiled-in default, which means CRM_DB is not set \
                 in this environment. deploy/configure.sh writes it to \
                 /etc/crm-light/app.env, which the systemd unit reads; running the \
                 binary by hand does not pick that file up. Either start it through \
                 systemd:\n    sudo systemctl start crm-light\n  or load the file \
                 yourself:\n    set -a; . /etc/crm-light/app.env; set +a; cargo run",
            );
        }
        message
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
            allow_signup: true,
            seed_demo_data: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_hides_the_password_and_admits_when_there_is_none() {
        assert_eq!(
            redact_url("postgresql://crm_light:hunter2@127.0.0.1:5432/db?sslmode=disable"),
            "postgresql://crm_light:***@127.0.0.1:5432/db?sslmode=disable"
        );
        // No password: say so by showing none, rather than inventing a `***`
        // that would imply a credential is present.
        assert_eq!(
            redact_url("postgresql://crm_light@127.0.0.1:5432/db"),
            "postgresql://crm_light@127.0.0.1:5432/db"
        );
        assert_eq!(
            redact_url("postgresql://crm_light:@127.0.0.1:5432/db"),
            "postgresql://crm_light:***@127.0.0.1:5432/db"
        );
        // Nothing recognisable: returned unchanged rather than mangled.
        assert_eq!(redact_url("sqlite:crm.db"), "sqlite:crm.db");
        assert_eq!(redact_url("postgresql://localhost/db"), "postgresql://localhost/db");
    }

    #[test]
    fn an_unusable_connection_url_is_explained_before_it_is_used() {
        let mut config = Config::for_tests("postgresql://crm_light@127.0.0.1:5432/db");
        let problem = config.check_database_url().expect_err("no password");
        assert!(problem.to_string().contains("no password"), "{problem}");

        config.database_url = "postgresql://crm_light:@127.0.0.1:5432/db".to_string();
        assert!(config.check_database_url().is_err(), "an empty password cannot authenticate");

        config.database_url = "postgresql://:pw@127.0.0.1:5432/db".to_string();
        assert!(config.check_database_url().is_err(), "an empty user cannot authenticate");

        config.database_url = "postgresql://crm_light:pw@127.0.0.1:5432/db".to_string();
        assert!(config.check_database_url().is_ok());
    }

    #[test]
    fn the_default_url_is_reported_as_unset_rather_than_wrong() {
        // The usual cause of this failure is not a typo but a missing
        // environment, so the message has to say so.
        let config = Config::for_tests(Config::DEFAULT_DATABASE_URL);
        let problem = config.check_database_url().expect_err("the default has no password");
        let problem = problem.to_string();
        assert!(problem.contains("not set in this environment"), "{problem}");
        assert!(problem.contains("app.env"), "{problem}");
        assert!(problem.contains("systemctl start crm-light"), "{problem}");
    }

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
