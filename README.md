# crm-light

A small multi-tenant CRM: companies, contacts, deals, and logged activity,
persisted in PostgreSQL, with workspaces, sign-up, and sign-in.

- **Web**: [Topcoat](https://github.com/tokio-rs/topcoat) 0.8 — server-rendered
  HTML with typed routes and components.
- **Persistence**: [Toasty](https://github.com/tokio-rs/toasty) 0.10 — an async
  Rust ORM, using its PostgreSQL driver.

## Running it

You need a PostgreSQL server and a database:

```bash
# macOS with Homebrew's postgresql@14:
brew services start postgresql@14

createdb crm_light
cargo run
```

Then open <http://127.0.0.1:3000> and **create a workspace at `/signup`**. You
become its administrator and are signed straight in.

There is no default account and no shared credential: sign-up is the only way
into a fresh installation. The first workspace starts empty, and the demo book
described below is seeded into it on the next start.

With `CRM_SEED` unset the app inserts demo data into the first workspace, so the
pages have something to show. It needs a workspace to put it in, so it waits
until somebody has signed up. Set `CRM_SEED=0` to skip it entirely.

### Configuration

| Variable                    | Default                                | Meaning                                                              |
| --------------------------- | -------------------------------------- | -------------------------------------------------------------------- |
| `CRM_DB`                    | `postgresql://localhost/crm_light`     | Toasty connection URL.                                                |
| `CRM_TZ`                    | the host's zone                        | IANA zone every timestamp is rendered in, e.g. `Europe/London`.       |
| `CRM_SECRET_KEY`            | generated per process                  | Cookie key; at least 64 bytes. Required for sessions to survive a restart. |
| `CRM_COOKIE_SECURE`         | `0`                                    | Set `1` behind TLS to add `Secure` to the session and CSRF cookies.    |
| `CRM_SESSION_TTL_HOURS`     | `336` (14 days)                        | How long a session stays valid.                                       |
| `CRM_PAGE_SIZE`             | `25`                                   | Rows per list page (1–500).                                           |
| `CRM_LOGIN_MAX_ATTEMPTS`    | `8`                                    | Failed sign-ins before an account is temporarily locked.              |
| `CRM_LOGIN_LOCKOUT_MINUTES` | `15`                                   | How long that lock lasts.                                             |
| `CRM_ALLOW_SIGNUP`          | `1`                                    | Whether a stranger may create a workspace at `/signup`. **Set to `0` on any server the internet can reach.** |
| `CRM_ALLOW_EMPTY_PASSWORD`  | `1`                                    | Whether an account with no stored password may sign in with a blank one. Nothing creates such an account, so this only matters for rows left by an older version. |
| `CRM_SEED`                  | unset (on)                             | Set to `0` to skip the demo data.                                     |
| `HOST`                      | `127.0.0.1`                            | Listen address (read by Topcoat).                                     |
| `PORT`                      | `3000`                                 | Listen port (read by Topcoat).                                        |

```bash
# Generate a key once and keep it: without it every restart signs everyone out
export CRM_SECRET_KEY="$(openssl rand -base64 64)"

# A throwaway instance with no demo data, on another port
CRM_DB=postgresql://localhost/crm_light_scratch CRM_SEED=0 PORT=4000 cargo run
```

A bad value — an unknown `CRM_TZ`, a short `CRM_SECRET_KEY`, a `CRM_PAGE_SIZE`
of `0` — stops the process at boot with a message naming the variable, rather
than being silently ignored.

### Tests

```bash
cargo test     # domain, access, auth, CSRF, pagination, and search unit tests
cargo clippy --all-targets
```

The tests are unit tests: they need no database server and run in about a
second. The ones worth knowing about pin the rules that are easy to break by
accident:

- `access::tests::every_tenant_owned_model_is_loadable` — fails if a tenant-owned
  model is added without going through the isolation check.
- `access::tests::attaching_a_reason_does_not_hide_the_status` — a 403 must stay
  a 403 once an explanation is attached, not become a 500.
- `auth::tests::assets_stay_anonymous_but_are_not_redirected_away` — the
  distinction that keeps the stylesheet loading after sign-in.
- `domain::tests::local_midnight_round_trips_through_a_dst_transition` — the
  date a user typed is the date they get back.

Anything that needs to issue a real query is exercised by hand against
PostgreSQL; see "What is not covered" below.

## What it does

| Page                  | What's there                                                        |
| --------------------- | ------------------------------------------------------------------- |
| `/signup`             | Create a workspace; you become its administrator (closable, see `CRM_ALLOW_SIGNUP`) |
| `/login`              | Sign-in: workspace, username, password, optional two-factor code      |
| `/`                   | Counts, pipeline value, pipeline by stage, paginated activity feed    |
| `/companies`          | Searchable, paginated list with contact counts and open pipeline      |
| `/companies/{id}`     | Details, contacts, deals, activity log                               |
| `/contacts`           | Searchable, paginated list across name, email, and job title          |
| `/contacts/{id}`      | Details, company, deals, activity log                                |
| `/deals`              | Paginated pipeline, filtered by title and stage                       |
| `/deals/{id}`         | Details, quick stage change, activity log                            |
| `/account`            | Your own password, two-factor settings, and live sessions             |
| `/admin/users`        | Account management (administrators only)                              |

Every record can be created, edited, and deleted. Activity is logged from the
page of whatever it belongs to, and posting returns you there. Every list page
is paginated and every filter runs in the database.

## Workspaces, and what one can see

A **workspace** is one organisation's set of records. Everything a member can see
belongs to exactly one workspace, and nobody can see into another.

```
Account ──< User ──< Session            the tenant, its people, their sessions
   └──< Company ──< Contact
              └──< Deal
                     └──< Activity      every CRM row carries account_id
```

Signing up creates a workspace and makes its first member an administrator. That
administrator adds the rest at `/admin/users`, and everybody in the workspace
shares its data — there is no per-record ownership *inside* a workspace.

### Isolation, and the two answers it gives

Every read of tenant-owned data goes through `src/access.rs`, which does two
things:

1. **Filters on `account_id`.** The filter is built in one place rather than
   retyped at each call site, because the failure mode of forgetting it is
   showing somebody another organisation's pipeline.
2. **Decides between 403 and 404.** A record that exists in *another* workspace
   answers **403**, with a page that names what was refused: "That company (id 1)
   belongs to a different workspace, so it is not yours to see or change." An id
   that is not there at all answers **404**.

The 403 is a deliberate choice. It tells a signed-in member that they have
reached the edge of their workspace rather than that they mistyped — the more
useful answer inside a CRM — at the cost of confirming that *some* row with that
id exists somewhere. If this ever held data whose mere existence is sensitive,
both branches should collapse into a 404; `Denied::into_error` in `src/access.rs`
is the one place to change it.

This applies to writes as much as reads. Naming a record from another workspace
in a form — a deal's company, a contact's company, an activity's subject — is
refused before the row is written, so a submission cannot forge a link between
two workspaces.

### Signing in

Usernames are unique **per workspace**, not globally: two organisations may each
have their own `admin` without colliding. That is why the sign-in form asks for
the workspace's slug as well as the username, and why `/signup` derives the slug
from the workspace name and shows you what it produced. The slug is prefilled
from your last sign-in on the same browser, so in practice it is typed once per
device.

Usernames are matched case-insensitively inside their workspace, and passwords
are stored as Argon2id PHC strings with a per-password salt, so two accounts with
the same password do not share a hash.

**Sign-up is public by default.** Anyone who can reach the port can create a
workspace; that is the point of a self-service installation, and it is also the
thing to turn off first if this is exposed to a network you do not control. Set
`CRM_ALLOW_SIGNUP=0` once your workspaces exist: `/signup` then answers 404 and
the login page stops advertising it. There is no invitation, email check, or
CAPTCHA in between — see `deploy/README.md` for the deployment order that closes
it.

### Roles

| Role            | May do within their workspace                               |
| --------------- | ---------------------------------------------------------- |
| **Member**      | Read and write every CRM record                             |
| **Administrator** | The same, plus `/admin/users`                              |

A role is a per-workspace thing: an administrator manages the accounts in their
own workspace and has no standing in any other. The role gates the administration
surface only — inside a workspace everybody sees everything, which is what "no
authorisation model" meant before.

### Two-factor authentication

Optional, per account, at `/account`. It is TOTP (RFC 6238): a base32 secret,
six digits, a 30-second step, and one step of clock drift allowed either way. The
secret is stored in plain text in the `users` row, because the server needs it to
check codes; anything with a database dump can generate valid codes, so the
database has to be protected rather than the column encrypted with a key that
sits next to it.

### Rules that keep a workspace administrable

All of these are scoped to one workspace, so an administrator in another cannot
keep one in place — or be kept in place by one.

- The last active administrator cannot be demoted, closed, or deleted.
- Nobody can demote, close, or delete their own account, so one careless click
  cannot lock you out — ask another administrator.
- An account that has logged activity is closed rather than deleted, so the
  audit trail keeps naming a real person. Deletion is offered only for an
  account with no history.
- Changing a password signs that account out everywhere except the session doing
  the change.
- Repeated failed sign-ins against one username lock it for a while, per
  workspace.

## Security

**Sessions.** A sign-in mints 256 bits of randomness, stores its SHA-256 digest
in the `sessions` row, and puts the token in an `HttpOnly`, `SameSite=Lax`
cookie. The database never holds a usable token, and signing out revokes the
session server-side rather than hoping the browser forgets. `/account` lists the
live sessions and can revoke the others.

**CSRF.** Every `POST` carries a token: a random value in a cookie, echoed in a
hidden field, compared in constant time. The check lives in the extractor rather
than in each handler, so a new `POST` route cannot be written without it — take
`csrf::CsrfForm<T>` and the token has already been verified. `SameSite=Lax` on
the session cookie blocks a cross-site `POST` outright in current browsers; the
token is the part that does not depend on browser behaviour.

**Passwords.** Argon2id with the default cost parameters. The password policy
requires length rather than character classes, and rejects a short blocklist.

**Open redirects.** `?next=…` on the login page is accepted only when it is a
same-origin absolute path, so the login form cannot be used to bounce a user to
another site.

**Response headers.** `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
and `Referrer-Policy: same-origin` are set on every authenticated response.

**Tenant isolation.** Every read of tenant-owned data goes through
`src/access.rs`; see "Workspaces" above. The filter is built in one place, and a
record belonging to another workspace is refused before it is returned or
written, including when a form names it as a company/contact/deal reference.

**Sign-in throttling.** Failed attempts are counted per workspace and username;
the counter clears on success and the account locks past the threshold. A failed
attempt against a username that does not exist — or a workspace that does not
exist — still costs a password verification, so neither response time nor the
message reveals which are real.

Setting `CRM_COOKIE_SECURE=1` and serving over TLS is still required for a real
deployment: without it the session cookie travels in clear text.

## Layout

```
src/
  lib.rs              module wiring, layout, stylesheet, `db(cx)`/`config_of(cx)`
  main.rs             startup: config → connect → migrate → seed → sweep → serve
  cli.rs              schema-management CLI (behind the `cli` feature)
  config.rs           environment parsing and validation
  models.rs           the Toasty models
  domain.rs           Stage/ActivityKind/Role, money, dates, time zones, LIKE escaping
  access.rs           tenancy: the tenant filter, 403-vs-404, slug and username rules
  auth.rs             password hashing, sessions, TOTP, throttling, the guard layer
  csrf.rs             token issuing and the validating form extractor
  flash.rs            one-shot notices carried across a redirect
  pagination.rs       opaque cursors, page assembly
  search.rs           case-insensitive, escape-aware text search
  seed.rs             the demo data
  views.rs            shared components: badges, activity feed, pager, error pages
  pages/
    mod.rs            shared page helpers, the catch-all 404
    signup.rs         /signup
    login.rs          /login, /logout, /account*
    admin.rs          /admin/users*
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

Money is an integer number of cents and timestamps are Unix seconds — no
floating point, and no database date type whose time zone handling would have to
be reasoned about separately.

```
accounts ──┬─< users ──< sessions
           │     └──< activities.user_id, companies.created_by, …   (who did it)
           ├─< login_attempts
           └─< companies ──┬─< contacts ──┐
                           ├─< deals ─────┤
                           └─< activities ┘
```

`activities.company_id` / `contact_id` / `deal_id` are all optional, so a note
can hang off a company, a contact, a deal, or nothing.

| Table            | Columns                                                                              |
| ---------------- | ------------------------------------------------------------------------------------ |
| `accounts`       | `id`, `name`, `slug`, `created_at`, `created_by`                                        |
| `users`          | `id`, **`account_id`**, `username`, `username_lower`, `display_name`, `password_hash`, `role`, `totp_secret`, `active`, `created_at`, `last_login_at` |
| `sessions`       | `id`, `token_hash`, **`account_id`**, `user_id`, `created_at`, `expires_at`, `revoked_at`, `user_agent`, `ip` |
| `login_attempts` | `id`, **`account_id`**, `username_lower`, `failures`, `locked_until`, `last_failure_at`  |
| `companies`      | `id`, **`account_id`**, `name`, `industry`, `website`, `phone`, `notes`, `created_at`, `created_by` |
| `contacts`       | `id`, **`account_id`**, `first_name`, `last_name`, `email`, `phone`, `title`, `company_id`, `notes`, `created_at`, `created_by` |
| `deals`          | `id`, **`account_id`**, `title`, `value_cents`, `stage`, `company_id`, `contact_id`, `expected_close`, `notes`, `created_at`, `created_by` |
| `activities`     | `id`, **`account_id`**, `kind`, `body`, `company_id`, `contact_id`, `deal_id`, `created_at`, `user_id`   |

**`account_id` is on every tenant-owned table**, and the tenant column is
deliberately *not* a `belongs_to` relation: a relation that eager-loads would
hide exactly the column that must never be forgotten.

`sessions.account_id` duplicates `users.account_id` on purpose. Resolving a
request is then one query rather than two, and a session can never be read as
belonging to a different workspace than its user — `resolve_session` refuses a
mismatch, which is redundant with how sessions are written and is kept because
it is the one place a mismatch would silently widen a tenant's reach.

`accounts.slug` is the sign-in name and is uniquely indexed, because sign-in has
to resolve a workspace from it.

`username_lower` is a separate, indexed column rather than a functional index, so
case-insensitive matching works the same way on any backend. It is **not**
globally unique: uniqueness is per workspace, and Toasty has no composite unique
index for root models, so `access::username_taken` enforces it and the index
keeps that check cheap.

Primary keys are `i64`, which is `BIGINT` in PostgreSQL and `INTEGER` in SQLite;
`#[auto]` becomes `GENERATED BY DEFAULT AS IDENTITY`.

Relationships between the CRM tables are plain indexed foreign-key columns
rather than `#[belongs_to]`/`#[has_many]`, so each page issues explicit queries
you can read top to bottom. Deleting a company, contact, or deal **detaches**
its children — it does not cascade — so a contact survives its company being
deleted, with `company_id` set back to `NULL`.

`stage` is one of `lead`, `qualified`, `proposal`, `negotiation`, `won`,
`lost`; `kind` is one of `note`, `call`, `email`, `meeting`; `role` is one of
`admin`, `member`. All are stored as text; `domain::Stage`,
`domain::ActivityKind`, and `domain::Role` are the typed wrappers, and an
unrecognised `role` degrades to `member` rather than to `admin`.

## Time zones

Timestamps are stored as Unix seconds — always UTC. Every value shown to a user
is rendered through the zone in `CRM_TZ`, so the same row reads correctly
wherever the app runs, and a DST transition never shifts a displayed time by an
hour.

That includes the dates people type. An expected close date of `2027-03-15` is
stored as the instant of **midnight in `CRM_TZ`** on that day, not midnight UTC,
so it displays back as the date that was typed. `parse_date_in` and
`format_date` are a matched pair for exactly that reason, and the DST cases are
covered by tests: on 2024-10-27 in London, local midnight is still BST.

If `CRM_TZ` is unset the host's zone is used. If it is set to something that is
not an IANA zone name, the process refuses to start rather than quietly
rendering every timestamp in the wrong place.

## Schema changes

The schema lives in `toasty/` and is applied at startup:

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
`cli` binary can also `migration apply`, `snapshot`, `drop`, and `reset`, and it
reads the same `CRM_DB` the server does.

`0001_extra_indexes.sql` is hand-written, because Toasty's `#[index]` creates a
single-column index per field and its root models have no composite index. It
carries two kinds of index: the `(sort column, id)` order every list page walks,
and tenant-first indexes for the `account_id` filter that leads almost every
query. Without the second, the planner has to choose between the tenant index
and the foreign-key index and filter afterwards.

## Deploying it

`deploy/` has what a remote Ubuntu server needs, and `deploy/README.md` is the
runbook:

| File | What it does |
| ---- | ------------ |
| `deploy/provision-db.sh` | Creates a non-superuser PostgreSQL role and a database it owns, generates a 32-character password, verifies the credentials authenticate over TCP, and writes the connection string to a root-only file. Idempotent; `--drop` destroys data and says so before doing it. |
| `deploy/configure.sh` | Creates the service account, generates `CRM_SECRET_KEY` (preserved across re-runs, so re-running does not sign everyone out), writes the environment file, and installs a hardened systemd unit. |
| `deploy/nginx/crm-light.conf` | TLS reverse proxy: HTTP/2, the redirect, rate limiting on `/login`, and forwarded headers. |

The short version:

```bash
sudo ./deploy/provision-db.sh
cargo build --release
sudo ./deploy/configure.sh --domain crm.example.com --allow-signup
sudo install -o crm-light -g crm-light -m 0755 target/release/crm-light /opt/crm-light/crm-light
sudo systemctl enable --now crm-light
# create your workspace at /signup, then close sign-up:
sudo ./deploy/configure.sh --domain crm.example.com && sudo systemctl restart crm-light
```

Neither script needs to know your password: `configure.sh` reads the connection
string out of the file `provision-db.sh` wrote, so it lives in one place.

## Notes and remaining limitations

The list of things a production CRM would still want:

- **No password reset by email.** An administrator sets passwords; there is no
  self-service reset and no mail.
- **The 403 leaks that a record exists somewhere.** It says "different
  workspace" rather than pretending the id is unknown. That is the requested
  behaviour and the more useful answer inside a CRM, but it is a deliberate
  trade; see "Isolation" above for how to collapse it into a 404.
- **No per-record ownership inside a workspace.** Everyone in a workspace reads
  and writes everything in it. The role decides only who manages accounts.
- **Sign-up is open until you close it.** There is no invitation, no email
  check, and no CAPTCHA; `CRM_ALLOW_SIGNUP=0` is the whole of the access
  control on creating a workspace.
- **A workspace cannot be renamed or deleted from the UI**, and there is no
  "leave workspace" path for a member. The slug is a sign-in credential, so
  changing it is a bigger operation than it looks.
- **No cross-workspace administration.** There is no superuser: nobody can see
  which workspaces exist, or help one that has lost its last administrator,
  short of editing the database.
- **Pagination is offset-based**, inside an opaque cursor. That removed the part
  that grew without bound — a page of 25 rows used to read every contact and
  every deal in the database to compute its rollups, and now reads only the rows
  on screen — but a large offset still makes the database walk the rows before
  it, and a row inserted above the current position shifts the window. True
  cursor pagination would fix both, at the cost of never being able to jump to a
  page number. The cursor names the column and direction it was minted under, so
  a stale link falls back to the first page instead of skipping rows.
- **No audit trail for edits.** Activity records who logged a note and who
  created a record, but an *edit* leaves no trace: changing a deal's value or a
  company's name is silent.
- **No rate limiting beyond sign-in.** The failed-login throttle covers `/login`;
  a signed-in account can POST as fast as it likes.
- **Search is a `LIKE` scan.** No full-text search, no ranking, and no index
  beyond the single-column ones — a search still reads the matching rows.
- **No tests that touch a database.** Every test is a unit test, so the SQL —
  the raw aggregate queries on the dashboard above all — is checked by hand
  rather than by CI.
- **PostgreSQL is the only backend this runs on.** The dashboard's aggregate
  queries are written in PostgreSQL SQL, because Toasty's typed API has `COUNT`
  but no `SUM` or `GROUP BY`; `search.rs` asks for `ILIKE`, which only
  PostgreSQL has. A non-`postgresql://` `CRM_DB` is refused at startup rather
  than failing later. The `sqlite` Toasty feature stays compiled in only so the
  migration CLI can generate DDL without a server.
- **Two-factor secrets are stored in plain text** in the `users` row, and there
  are no single-use recovery codes: losing the authenticator means an
  administrator has to clear the secret.
