//! crm-light — a small CRM built with Topcoat (web) and Toasty (SQLite).
//!
//! The crate is a library so the migration CLI (`src/bin/cli.rs`) can register
//! the same models the server uses. The binary is a thin startup wrapper.

// Topcoat components receive every value they render as a named argument, so
// the create/edit forms legitimately exceed clippy's default arity threshold.
#![allow(clippy::too_many_arguments)]

pub mod domain;
pub mod models;
pub mod pages;
pub mod seed;

use toasty::Db;
use topcoat::{
    Result,
    context::{Cx, app_context},
    router::{Slot, content::Css, layout, route},
    view::{View, view},
};

/// Toasty statements need a mutable handle; `Db` is a cheap handle to a pool.
pub fn db(cx: &Cx) -> Db {
    app_context::<Db>(cx).clone()
}

/// The single layout wrapping every page.
#[layout("/")]
async fn root(slot: Slot<'_>) -> Result<impl View> {
    Ok(view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>"crm-light"</title>
                <link rel="stylesheet" href="/style.css">
                topcoat::dev::script()
            </head>
            <body>
                <header class="topbar">
                    <a class="brand" href="/">"crm-light"</a>
                    <nav>
                        <a href="/">"Dashboard"</a>
                        <a href="/companies">"Companies"</a>
                        <a href="/contacts">"Contacts"</a>
                        <a href="/deals">"Deals"</a>
                    </nav>
                </header>
                <main>(slot)</main>
            </body>
        </html>
    })
}

/// Stylesheet, served as `text/css` from the router.
#[route(GET "/style.css")]
async fn style() -> Result<Css<&'static str>> {
    Ok(Css(STYLESHEET))
}

const STYLESHEET: &str = r#"
:root {
  --bg: #f6f7f9;
  --panel: #ffffff;
  --ink: #1c2024;
  --muted: #6b7280;
  --line: #e3e6ea;
  --accent: #2f6fed;
  --accent-ink: #ffffff;
  --danger: #c0392b;
}

* { box-sizing: border-box; }

body {
  margin: 0;
  background: var(--bg);
  color: var(--ink);
  font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
}

a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }

.topbar {
  display: flex;
  align-items: center;
  gap: 1.5rem;
  padding: 0.75rem 1.5rem;
  background: var(--panel);
  border-bottom: 1px solid var(--line);
}
.topbar .brand { font-weight: 700; color: var(--ink); letter-spacing: -0.01em; }
.topbar nav { display: flex; gap: 1rem; }
.topbar nav a { color: var(--muted); font-weight: 500; }
.topbar nav a:hover { color: var(--accent); }

main { max-width: 1100px; margin: 0 auto; padding: 1.5rem; }

.page-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 1rem;
  margin-bottom: 1rem;
}
h1 { font-size: 1.5rem; margin: 0; letter-spacing: -0.01em; }
h2 { font-size: 1.1rem; margin: 1.5rem 0 0.5rem; }

.panel {
  background: var(--panel);
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 1rem;
  margin-bottom: 1rem;
}

.grid { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); }
.stat { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 1rem; }
.stat .value { font-size: 1.6rem; font-weight: 700; letter-spacing: -0.02em; }
.stat .label { color: var(--muted); font-size: 0.85rem; text-transform: uppercase; letter-spacing: 0.04em; }

table { width: 100%; border-collapse: collapse; background: var(--panel); }
th, td { text-align: left; padding: 0.6rem 0.75rem; border-bottom: 1px solid var(--line); }
th { font-size: 0.78rem; text-transform: uppercase; letter-spacing: 0.04em; color: var(--muted); }
tbody tr:last-child td { border-bottom: none; }
tbody tr:hover { background: #fafbfc; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }

.btn {
  display: inline-block;
  padding: 0.4rem 0.8rem;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--panel);
  color: var(--ink);
  font: inherit;
  cursor: pointer;
}
.btn:hover { background: #f2f4f7; text-decoration: none; }
.btn-primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); }
.btn-primary:hover { background: #245cd0; }
.btn-danger { color: var(--danger); }
.btn-sm { padding: 0.2rem 0.5rem; font-size: 0.85rem; }

form.inline { display: inline; }

.field { margin-bottom: 0.75rem; display: flex; flex-direction: column; gap: 0.25rem; }
.field label { font-size: 0.85rem; color: var(--muted); }
input, select, textarea {
  font: inherit;
  padding: 0.45rem 0.6rem;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: #fff;
  color: var(--ink);
  width: 100%;
}
textarea { min-height: 5rem; resize: vertical; }
.form-row { display: grid; gap: 0.75rem; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); }

.searchbar { display: flex; gap: 0.5rem; margin-bottom: 1rem; }
.searchbar input { max-width: 22rem; }

.badge {
  display: inline-block;
  padding: 0.1rem 0.5rem;
  border-radius: 999px;
  font-size: 0.78rem;
  font-weight: 600;
  border: 1px solid transparent;
}
.stage-lead { background: #eef1f5; color: #445; }
.stage-qualified { background: #e3f0ff; color: #14508c; }
.stage-proposal { background: #eae4ff; color: #4b2fa3; }
.stage-negotiation { background: #fff1d6; color: #8a5a00; }
.stage-won { background: #e0f5e6; color: #1c6b38; }
.stage-lost { background: #fbe4e2; color: #9b2c20; }

.muted { color: var(--muted); }
.empty { color: var(--muted); padding: 1rem 0; font-style: italic; }
.actions { display: flex; gap: 0.4rem; align-items: center; }

dl.meta { display: grid; grid-template-columns: 9rem 1fr; gap: 0.35rem 1rem; margin: 0; }
dl.meta dt { color: var(--muted); font-size: 0.88rem; }
dl.meta dd { margin: 0; }

.activity { border-bottom: 1px solid var(--line); padding: 0.6rem 0; }
.activity:last-child { border-bottom: none; }
.activity .head { display: flex; justify-content: space-between; gap: 1rem; color: var(--muted); font-size: 0.85rem; }
.activity .body { white-space: pre-wrap; }

.cols { display: grid; gap: 1rem; grid-template-columns: 2fr 1fr; align-items: start; }
@media (max-width: 800px) { .cols { grid-template-columns: 1fr; } }
"#;
