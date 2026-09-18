# crm-light

A small CRM: companies, contacts, deals, and logged activity, persisted in
SQLite.

- **Web**: [Topcoat](https://github.com/tokio-rs/topcoat) 0.8 — server-rendered
  HTML with typed routes and components.
- **Persistence**: [Toasty](https://github.com/tokio-rs/toasty) 0.10 — an async
  Rust ORM, using its SQLite driver.

## Running it

```bash
cargo run
```

Then open <http://127.0.0.1:3000>.

On first start the app applies its migrations and, if the database has no
companies yet, inserts demo data so the pages have something to show. Delete
`crm.db` to start over.

### Configuration

| Variable   | Default         | Meaning                                              |
| ---------- | --------------- | ---------------------------------------------------- |
| `CRM_DB`   | `sqlite:crm.db` | Toasty connection URL. `sqlite::memory:` also works.  |
| `CRM_SEED` | unset (on)      | Set to `0` to skip the demo data.                     |
| `HOST`     | `127.0.0.1`     | Listen address (read by Topcoat).                     |
| `PORT`     | `3000`          | Listen port (read by Topcoat).                        |

```bash
# A throwaway in-memory instance, no demo data
CRM_DB=sqlite::memory: CRM_SEED=0 cargo run

# A separate database on another port
CRM_DB=sqlite:/tmp/other.db PORT=4000 cargo run
```

### Tests

```bash
cargo test     # unit tests for money, date, and stage handling
cargo clippy --all-targets
```

## What it does

| Page                  | What's there                                                        |
| --------------------- | ------------------------------------------------------------------- |
| `/`                   | Counts, pipeline value, pipeline by stage, recent activity           |
| `/companies`          | Searchable list with contact counts and open pipeline per company    |
| `/companies/{id}`     | Details, contacts, deals, activity log                               |
| `/contacts`           | Searchable list across name, email, and job title                    |
| `/contacts/{id}`      | Details, company, deals, activity log                                |
| `/deals`              | Pipeline list, filtered by title and stage                           |
| `/deals/{id}`         | Details, quick stage change, activity log                            |

Every record can be created, edited, and deleted. Activity is logged from the
page of whatever it belongs to, and posting returns you there.

## Layout

```
src/
  lib.rs              module wiring, shared layout, stylesheet, `db(cx)` helper
  main.rs             startup: connect → migrate → seed → serve
  bin/cli.rs          schema-management CLI (behind the `cli` feature)
  models.rs           the four Toasty models
  domain.rs           Stage/ActivityKind, money and date helpers
  seed.rs             demo data
  pages/
    mod.rs            shared components (stage badge, activity feed + form)
    dashboard.rs      /
    companies.rs      /companies*
    contacts.rs       /contacts*
    deals.rs          /deals*
    activities.rs     POST /activities*
```

The crate is a library plus two binaries. It has to be, because
`toasty::models!(...)` discovers models per crate and the migration CLI needs
the same model set the server registers.

## Data model

Four tables. Money is an integer number of cents and timestamps are Unix
seconds — no floating point, no timezone library.

```
companies ──┬─< contacts ──┐
            ├─< deals ─────┤
            └─< activities ┘        activities.company_id / contact_id / deal_id
                                    are all optional, so a note can hang off
                                    a company, a contact, a deal, or nothing
```

| Table        | Columns                                                                              |
| ------------ | ------------------------------------------------------------------------------------ |
| `companies`  | `id`, `name`, `industry`, `website`, `phone`, `notes`, `created_at`                    |
| `contacts`   | `id`, `first_name`, `last_name`, `email`, `phone`, `title`, `company_id`, `notes`, `created_at` |
| `deals`      | `id`, `title`, `value_cents`, `stage`, `company_id`, `contact_id`, `expected_close`, `notes`, `created_at` |
| `activities` | `id`, `kind`, `body`, `company_id`, `contact_id`, `deal_id`, `created_at`              |

Relationships are plain indexed foreign-key columns rather than
`#[belongs_to]`/`#[has_many]` relations, so each page issues explicit queries
you can read top to bottom. Deleting a company, contact, or deal **detaches**
its children — it does not cascade — so a contact survives its company being
deleted, with `company_id` set back to `NULL`.

`stage` is one of `lead`, `qualified`, `proposal`, `negotiation`, `won`,
`lost`; `kind` is one of `note`, `call`, `email`, `meeting`. Both are stored as
text; `domain::Stage` and `domain::ActivityKind` are the typed wrappers.

## Schema changes

The schema lives in `toasty/` and is applied at startup. To change it:

```bash
# 1. Edit or add a #[derive(toasty::Model)] struct in src/models.rs
# 2. Generate a migration from the diff against the stored snapshot
cargo run --features cli --bin crm-light-cli -- migration generate

# 3. Restart the server; pending migrations are applied automatically
cargo run
```

Migrations are compiled into the binary with `embed_migrations!`, and applied
migrations are recorded in the database's `__toasty_migrations` table, so
`apply` is idempotent — restarting never re-runs or re-creates anything. The
`cli` binary can also `migration apply`, `snapshot`, `drop`, and `reset`.

> An early version of this app called `db.push_schema()`, which emits plain
> `CREATE TABLE` statements and therefore panics on the second start. If you
> have a `crm.db` from that version (its only tables are `companies` and
> `sqlite_sequence`, with no `__toasty_migrations`), delete it and let the
> migrations build a fresh one.

## Notes and limitations

It is deliberately small, and a few things a production CRM would need are
absent:

- **No authentication.** Anyone who can reach the port can read and write
  everything.
- **No CSRF protection** on the POST forms, and no authorisation model.
- **No pagination.** Lists load every row; the search filters run in SQL, but
  the per-company rollups on `/companies` and `/deals` are computed in memory
  from one bulk read rather than one query per row.
- **`like` search** is case-insensitive for ASCII on SQLite by default, and the
  `%` and `_` characters in a search term are treated as wildcards.
- **Timestamps are UTC**, formatted by `domain.rs` rather than by a date
  library, so there is no local-timezone handling.
