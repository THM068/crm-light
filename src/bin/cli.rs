//! Migration CLI, built with `--features cli`.
//!
//! ```text
//! cargo run --features cli --bin crm-light-cli -- migration generate
//! cargo run --features cli --bin crm-light-cli -- migration apply
//! ```
//!
//! `migration generate` diffs the models against the stored snapshot and writes
//! a SQL file under `toasty/`; `apply` runs anything still pending. The server
//! itself applies the embedded migrations at startup, so `apply` is only needed
//! when you want to migrate a database without booting the web app.

use toasty_cli::{Config, ToastyCli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("CRM_DB").unwrap_or_else(|_| "sqlite:crm.db".to_string());

    let db = toasty::Db::builder()
        .models(toasty::models!(crm_light::*))
        .connect(&url)
        .await?;

    let cli = ToastyCli::with_config(db, Config::load()?);
    cli.parse_and_run().await?;

    Ok(())
}
