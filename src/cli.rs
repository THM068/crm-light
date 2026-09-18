//! Migration CLI, built with `--features cli`.
//!
//! ```text
//! cargo run --features cli --bin crm-light-cli -- migration generate
//! cargo run --features cli --bin crm-light-cli -- migration apply
//! cargo run --features cli --bin crm-light-cli -- migration snapshot
//! ```
//!
//! `migration generate` diffs the models against the stored snapshot and writes
//! a SQL file under `toasty/`; `apply` runs anything still pending. The server
//! itself applies the embedded migrations at startup, so `apply` is only needed
//! when you want to migrate a database without booting the web app.
//!
//! The connection URL comes from `CRM_DB` and defaults to the same PostgreSQL
//! database the server uses, so the CLI and the server always agree on which
//! database they are talking about.

use toasty_cli::{Config, ToastyCli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("CRM_DB")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crm_light::config::Config::DEFAULT_DATABASE_URL.to_string());

    let db = toasty::Db::builder()
        .models(toasty::models!(crm_light::*))
        .connect(&url)
        .await?;

    let cli = ToastyCli::with_config(db, Config::load()?);
    cli.parse_and_run().await?;

    Ok(())
}
