//! Process configuration, read once at startup from the environment.
//!
//! Nothing else in the app reads `std::env`: every knob is parsed and validated
//! here, so a misconfigured deployment fails loudly at boot rather than at the
//! first request.

use std::collections::HashMap;
use std::sync::OnceLock;
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

    /// Where AI briefings come from, and what they may cost.
    pub ai: crate::ai::AiConfig,
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

/// Values read from a `.env` file, looked at only when the real environment has
/// nothing to say.
///
/// A `OnceLock` rather than a parameter threaded through every reader, so the
/// loader stays a detail of this module: nothing outside it needs to know that
/// `.env` exists.
static DOTENV: OnceLock<HashMap<String, String>> = OnceLock::new();

/// The name of the file, and the one variable that can point somewhere else.
const DOTENV_PATH: &str = ".env";
const DOTENV_PATH_VAR: &str = "CRM_ENV_FILE";

/// Read one setting.
///
/// Precedence is real environment first, then `.env`, then the caller's default.
/// The environment has to win so that `CRM_DB=... cargo run` and the systemd
/// unit's `EnvironmentFile` both override a checked-out `.env` rather than being
/// silently overridden by it.
fn env(name: &str) -> Option<String> {
    if let Ok(value) = std::env::var(name)
        && !value.trim().is_empty()
    {
        return Some(value);
    }
    dotenv().get(name).cloned()
}

/// The parsed `.env`, read once per process.
fn dotenv() -> &'static HashMap<String, String> {
    DOTENV.get_or_init(|| {
        let path = std::env::var(DOTENV_PATH_VAR).unwrap_or_else(|_| DOTENV_PATH.to_string());
        match std::fs::read_to_string(&path) {
            Ok(contents) => parse_dotenv(&contents),
            // Absent is the normal case for a server, where the systemd unit
            // supplies the environment instead.
            Err(_) => HashMap::new(),
        }
    })
}

/// Parse the subset of `.env` syntax that people actually write.
///
/// `KEY=value`, blank lines, `#` comments, an optional `export ` prefix, and
/// surrounding single or double quotes. Deliberately not a shell: no expansion,
/// no command substitution, no multi-line values. A password written here is
/// therefore taken literally, which is the property that matters — a `$` or a
/// backtick in a password must not do anything surprising.
fn parse_dotenv(contents: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }

        let value = value.trim();
        // A value is quoted only when the *same* character opens and closes it,
        // and there is something in between. Testing the opening quote alone
        // stripped one side of `"  spaced  "` — whose closing quote is not the
        // last character, because of the trailing space — and left a stray `"`.
        let quoted = [('"', '"'), ('\'', '\'')]
            .into_iter()
            .find(|(open, close)| {
                value.len() >= 2 && value.starts_with(*open) && value.ends_with(*close)
            });
        let value = match quoted {
            Some((_, close)) => &value[1..value.len() - close.len_utf8()],
            // Unquoted: a ` #` starts a trailing comment, so a password
            // containing one has to be quoted to survive. Documented in
            // .env.example, because it is the one genuinely surprising rule.
            None => value.split(" #").next().unwrap_or(value).trim(),
        };

        values.insert(key.to_string(), value.to_string());
    }

    values
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

        let ai_timeout_secs = env_i64("CRM_AI_TIMEOUT_SECONDS", 45)?;
        if !(5..=300).contains(&ai_timeout_secs) {
            return Err(ConfigError(
                "CRM_AI_TIMEOUT_SECONDS must be between 5 and 300".to_string(),
            ));
        }

        let ai_max_tokens = env_i64("CRM_AI_MAX_TOKENS", 700)?;
        if !(64..=8192).contains(&ai_max_tokens) {
            return Err(ConfigError(
                "CRM_AI_MAX_TOKENS must be between 64 and 8192".to_string(),
            ));
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

        // The key is the on/off switch: without it the feature is unavailable
        // rather than failing when somebody presses the button. There is no
        // default key and none is read from anywhere but the environment, so
        // this is off unless somebody deliberately turns it on.
        let ai = crate::ai::AiConfig {
            api_key: env("CRM_AI_API_KEY"),
            model: env("CRM_AI_MODEL")
                .unwrap_or_else(|| crate::ai::AiConfig::DEFAULT_MODEL.to_string()),
            base_url: env("CRM_AI_BASE_URL")
                .unwrap_or_else(|| crate::ai::AiConfig::DEFAULT_BASE_URL.to_string()),
            timeout: Duration::from_secs(ai_timeout_secs as u64),
            max_tokens: ai_max_tokens as u32,
        };
        if ai.is_enabled() {
            warnings.push(format!(
                "CRM_AI_API_KEY is set, so \"Generate AI briefing\" is available and sends a \
                 contact's name, notes, deals, and recent activity to {} ({}) when somebody \
                 presses it. That is customer data leaving this server.",
                ai.base_url, ai.model
            ));
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
            ai,
        };

        Ok((config, ConfigWarnings(warnings)))
    }

    /// Whether the connection URL is one this app can authenticate with, and
    /// why not if it is not.
    ///
    /// # What is actually required
    ///
    /// The Postgres driver reports a URL it cannot use as "invalid
    /// configuration: password missing", which names neither the variable nor
    /// the file. Checking here turns that into a message that does.
    ///
    /// The rule is narrower than "must have a password", and an earlier version
    /// of this got it wrong: **a password is required when connecting over TCP,
    /// and not otherwise.** A URL with no host at all —
    /// `postgresql:///crm_light`, or the driver's shorthand
    /// `postgresql://localhost/crm_light` — connects over a unix socket, where
    /// peer authentication may legitimately need no password at all. That is
    /// exactly how a developer's local cluster is usually set up, so demanding
    /// a password there broke local development to protect a deployment that
    /// was never the case in question.
    ///
    /// # Errors
    ///
    /// Returns a message naming what is missing and how to supply it.
    pub fn check_database_url(&self) -> Result<(), ConfigError> {
        let url = &self.database_url;

        let Some((_scheme, rest)) = url.split_once("://") else {
            // Not a URL shape this understands; `supports` rejects the scheme.
            return Ok(());
        };

        // Everything up to the first `/` is the authority: `user:pass@host:port`
        // or empty for a socket. A password or user containing `/` would be
        // percent-encoded, so splitting here is safe.
        let authority = rest.split('/').next().unwrap_or_default();
        let (userinfo, host) = match authority.rsplit_once('@') {
            Some((userinfo, host)) => (Some(userinfo), host),
            None => (None, authority),
        };

        // A host with no address is the socket case: `localhost` is the
        // driver's shorthand for the default unix socket path.
        let uses_socket = host.is_empty() || host.starts_with("localhost") || host.starts_with('/');

        let Some(userinfo) = userinfo else {
            if uses_socket {
                return Ok(());
            }
            return Err(ConfigError(self.unusable_url_message(
                "it connects over TCP but has no user name or password",
            )));
        };

        let (user, password) = match userinfo.split_once(':') {
            Some((user, password)) => (user, Some(password)),
            None => (userinfo, None),
        };

        if user.is_empty() {
            return Err(ConfigError(
                self.unusable_url_message("the user name is empty"),
            ));
        }

        match password {
            Some("") => Err(ConfigError(
                self.unusable_url_message("the password is empty"),
            )),
            Some(_) => Ok(()),
            None if uses_socket => Ok(()),
            None => Err(ConfigError(self.unusable_url_message(
                "it connects over TCP but has no password",
            ))),
        }
    }

    /// The explanation for a URL that cannot authenticate, with the fix for
    /// whichever situation the value suggests.
    fn unusable_url_message(&self, problem: &str) -> String {
        let mut message = format!(
            "CRM_DB is not usable: {problem}.\n\
             \n\
             CRM_DB is currently: {}\n\
             \n\
             A TCP connection carries the credentials before the host:\n  \
             postgresql://USER:PASSWORD@127.0.0.1:5432/crm_light?sslmode=disable\n\
             \n\
             A unix-socket connection needs no password where the server uses peer\
             authentication, which is how a local cluster is normally set up:\n  \
             postgresql://localhost/crm_light",
            redact_url(&self.database_url)
        );

        if self.database_url == Self::DEFAULT_DATABASE_URL {
            message.push_str(
                "\n\
                 \n\
                 That is the compiled-in default, which means CRM_DB is not set in this\
                 environment. For local development, put it in a .env file in the\
                 repository root and it is read automatically:\n  \
                 cp .env.example .env        # then edit, or run deploy/provision-db.sh --out .env\n\
                 \n\
                 On a server it is the systemd unit that supplies it:\n  \
                 sudo systemctl start crm-light",
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
            ai: crate::ai::AiConfig {
                api_key: None,
                model: crate::ai::AiConfig::DEFAULT_MODEL.to_string(),
                base_url: crate::ai::AiConfig::DEFAULT_BASE_URL.to_string(),
                timeout: Duration::from_secs(5),
                max_tokens: 256,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_parsing_takes_values_literally() {
        let parsed = parse_dotenv(
            "\n# a comment\n\
             CRM_DB=postgresql://u:p@localhost/db\n\
             export CRM_TZ=Europe/London\n\
             QUOTED=\"  spaced  \"\n\
             SINGLE='single quoted'\n\
             TRAILING=value # a comment\n\
             HASH_IN_VALUE=abc #def\n\
             =novalue\n\
             NOT_A_PAIR\n\
             EMPTY=\n",
        );

        assert_eq!(
            parsed.get("CRM_DB").map(String::as_str),
            Some("postgresql://u:p@localhost/db")
        );
        assert_eq!(parsed.get("CRM_TZ").map(String::as_str), Some("Europe/London"));
        assert_eq!(parsed.get("QUOTED").map(String::as_str), Some("  spaced  "));
        assert_eq!(parsed.get("SINGLE").map(String::as_str), Some("single quoted"));
        assert_eq!(parsed.get("TRAILING").map(String::as_str), Some("value"));
        // An unquoted `#` starts a comment, so this keeps only `abc`.
        assert_eq!(parsed.get("HASH_IN_VALUE").map(String::as_str), Some("abc"));
        assert_eq!(parsed.get("EMPTY").map(String::as_str), Some(""));
        assert!(!parsed.contains_key("NOT_A_PAIR"));
        assert!(!parsed.contains_key(""));
    }

    #[test]
    fn a_password_with_shell_metacharacters_survives_dotenv_parsing() {
        // The whole point of not being a shell: these must be taken literally
        // rather than expanded, substituted, or treated as comments.
        let parsed = parse_dotenv(
            "CRM_DB=postgresql://u:a$b`c\\d\"e'f@g/h?x=1\nCRM_SECRET_KEY=has$dollar\n",
        );
        assert_eq!(
            parsed.get("CRM_DB").map(String::as_str),
            Some("postgresql://u:a$b`c\\d\"e'f@g/h?x=1")
        );
        assert_eq!(parsed.get("CRM_SECRET_KEY").map(String::as_str), Some("has$dollar"));
    }

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
    fn a_socket_connection_needs_no_password() {
        // The regression that broke local development: a unix-socket URL with
        // peer authentication is valid, and demanding a password for it made
        // `cargo run` unusable on a developer's machine.
        for url in [
            Config::DEFAULT_DATABASE_URL,
            "postgresql://localhost/crm_light",
            "postgresql:///crm_light",
            "postgresql:///var/run/postgresql/crm_light",
            "postgresql://admin@localhost/crm_light",
        ] {
            let config = Config::for_tests(url);
            assert!(
                config.check_database_url().is_ok(),
                "{url} should be usable without a password: {:?}",
                config.check_database_url()
            );
        }
    }

    #[test]
    fn a_tcp_connection_without_a_password_is_explained() {
        let mut config = Config::for_tests("postgresql://crm_light@127.0.0.1:5432/db");
        let problem = config.check_database_url().expect_err("no password");
        assert!(problem.to_string().contains("no password"), "{problem}");

        config.database_url = "postgresql://crm_light:@127.0.0.1:5432/db".to_string();
        assert!(
            config.check_database_url().is_err(),
            "an empty password cannot authenticate over TCP"
        );

        config.database_url = "postgresql://:pw@127.0.0.1:5432/db".to_string();
        assert!(config.check_database_url().is_err(), "an empty user cannot authenticate");

        config.database_url = "postgresql://crm_light:pw@127.0.0.1:5432/db".to_string();
        assert!(config.check_database_url().is_ok());
    }

    #[test]
    fn a_broken_url_says_where_the_value_should_come_from() {
        // When the value is the compiled-in default, the cause is almost always
        // a missing environment rather than a typo, so the message has to name
        // the file to put it in and the command for each context.
        let mut config = Config::for_tests(Config::DEFAULT_DATABASE_URL);
        // The default is usable (socket), so make it clearly broken to reach the
        // message that has to explain itself.
        config.database_url = "postgresql://@127.0.0.1:5432/crm_light".to_string();
        let problem = config.check_database_url().expect_err("no user").to_string();
        assert!(problem.contains("CRM_DB is currently"), "{problem}");
        assert!(problem.contains("postgresql://USER:PASSWORD@"), "{problem}");

        // And the default-valued case gets the extra guidance.
        let default_config = Config::for_tests(Config::DEFAULT_DATABASE_URL);
        assert!(
            default_config.check_database_url().is_ok(),
            "the compiled-in default must work for local development"
        );
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
