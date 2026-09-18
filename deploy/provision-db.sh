#!/usr/bin/env bash
#
# Create the PostgreSQL role and database this app needs, on the machine the app
# will run on, and write the resulting connection string to a file for the
# service to read.
#
# Run it on the server, as a user who can `sudo -u postgres`:
#
#     sudo ./deploy/provision-db.sh
#     sudo ./deploy/provision-db.sh --db crm_prod --role crm_app --out /etc/crm-light/env
#
# It is idempotent: running it again rotates the password and leaves the data
# alone. Dropping anything requires --drop, which is deliberately a separate,
# explicit decision.
#
# What it creates, and why each part is the way it is:
#
#   * A dedicated NON-superuser role. The application never needs to create
#     roles or databases, so it does not get to. `CREATEDB`/`CREATEROLE` are
#     left off deliberately: they are the privileges that turn a leaked
#     connection string into a takeover of the whole cluster.
#
#   * A database OWNED BY that role, so the app can create its own tables and
#     indexes at startup without a superuser anywhere in the picture.
#
#   * A password of 32 random alphanumeric characters. Alphanumeric only,
#     because the password is embedded in a URL: a `:` or `/` in it would need
#     percent-encoding, and "it worked on my machine" is not what you want from
#     a connection string. 32 characters of that alphabet is about 190 bits.
#
#   * `ALTER ROLE ... SET password_encryption`, not needed — PostgreSQL 14 on
#     Ubuntu already defaults to scram-sha-256, and tokio-postgres speaks it.
#
#   * The connection host is 127.0.0.1, not a unix socket path. The app connects
#     with a host and a password, which is the shape that also works unchanged
#     if the database ever moves to another machine.
#
# Nothing here opens the database to the network. On a stock Ubuntu install
# PostgreSQL listens on localhost only; do not change that for a single-host
# deployment — the app and the database are on the same box, so they can talk
# over the loopback interface and never touch a wire.

set -euo pipefail

DB_NAME="crm_light"
DB_ROLE="crm_light"
DB_HOST="127.0.0.1"
DB_PORT="5432"
OUT_FILE="/etc/crm-light/db.env"
DROP="no"

usage() {
    cat <<'USAGE'
Usage: sudo ./deploy/provision-db.sh [options]

  --db NAME      database to create            (default: crm_light)
  --role NAME    role that will own it         (default: crm_light)
  --host HOST    host the app connects to      (default: 127.0.0.1)
  --port PORT    port the app connects to      (default: 5432)
  --out PATH     where to write the env file   (default: /etc/crm-light/db.env)
  --drop         ALSO DROP the database and role first, destroying all data
  -h, --help     this message

Re-running without --drop rotates the role's password and keeps the data.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --db) DB_NAME="$2"; shift 2 ;;
        --role) DB_ROLE="$2"; shift 2 ;;
        --host) DB_HOST="$2"; shift 2 ;;
        --port) DB_PORT="$2"; shift 2 ;;
        --out) OUT_FILE="$2"; shift 2 ;;
        --drop) DROP="yes"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

die() { echo "provision-db: $*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || [ -n "${CRM_PG_SUPERUSER:-}" ] \
    || die "run this with sudo (it needs to act as the postgres user)"
command -v psql >/dev/null 2>&1 || die "psql not found; install postgresql first (apt install postgresql)"
command -v openssl >/dev/null 2>&1 || die "openssl not found; install it (apt install openssl)"

# Refuse names that would need quoting in SQL. The defaults are plain ASCII, and
# so should anything passed in.
for name in "$DB_NAME" "$DB_ROLE"; do
    case "$name" in
        *[!A-Za-z0-9_]*) die "'$name' may only contain letters, digits, and underscores" ;;
    esac
    [ -n "$name" ] || die "names may not be empty"
done

# `sudo -u postgres psql` is how the installer, and every guide, reaches the
# cluster: it uses peer authentication over the unix socket. OVERRIDES outside
# the shell so a stray PGHOST in the environment cannot redirect it.
#
# Setting CRM_PG_SUPERUSER names a role to connect as instead, which is how this
# is exercised in a test: on a cluster where you already are a superuser, no
# `sudo` is involved. Nothing about the deployment path changes.
#
# `-d postgres` is explicit rather than left to the default: psql picks the
# database named after the connecting role when none is given, and a role
# without a same-named database — a superuser called `admin`, say — fails before
# it runs anything. A later `-d "$DB_NAME"` on the command line overrides it,
# since psql takes the last one.
if [ -n "${CRM_PG_SUPERUSER:-}" ]; then
    echo "note: connecting as '${CRM_PG_SUPERUSER}' (CRM_PG_SUPERUSER) rather than via sudo"
    as_postgres() {
        psql -U "$CRM_PG_SUPERUSER" -d postgres -v ON_ERROR_STOP=1 "$@"
    }
else
    as_postgres() {
        sudo -u postgres env -u PGHOST -u PGPORT -u PGDATABASE -u PGUSER \
            psql -d postgres -v ON_ERROR_STOP=1 "$@"
    }
fi

if [ "$DROP" = "yes" ]; then
    echo "!! --drop was given: DROPPING database '$DB_NAME' and role '$DB_ROLE'."
    echo "!! Every record in that database will be gone. Sleeping 5s; Ctrl-C to stop."
    sleep 5
    # WITH (FORCE) disconnects clients, so this works while the app is running.
    as_postgres -c "DROP DATABASE IF EXISTS \"$DB_NAME\" WITH (FORCE);"
    as_postgres -c "DROP ROLE IF EXISTS \"$DB_ROLE\";"
fi

# --- Password -------------------------------------------------------------
#
# In a variable, never on a command line: arguments are visible to every user on
# the machine through /proc, and this one is the database's front door.
DB_PASSWORD="$(openssl rand -base64 48 | tr -dc 'A-Za-z0-9' | head -c 32)"
[ "${#DB_PASSWORD}" -eq 32 ] || die "could not generate a password"

# --- Role -----------------------------------------------------------------
#
# Two things this deliberately does NOT do, both of which broke an earlier
# version of this script:
#
#   * No `format(...)` with `%I` / `%L`. `format` consumes exactly as many
#     arguments as the format string has specs and ignores any extras, so
#     passing one argument too many does not fail — it silently shifts every
#     value one place along, and the password ends up being the role name. On
#     some builds the same mismatch is a syntax error at the first `%` instead.
#     Either way it is the wrong tool: psql can do the quoting itself.
#
#   * No `\gexec` building SQL from a query. psql's `\if` says what is meant.
#
# `:"role"` quotes an identifier and `:'password'` quotes a string literal, both
# handled by psql *after* variable substitution, which is what makes this safe
# with a password containing quotes and correct regardless of server version.
#
# `-v password=...` goes on the command line for psql itself, not for the
# server: psql's own arguments are not visible in the server's logs or in
# `pg_stat_activity`, unlike a value interpolated into a query text.
echo "role '$DB_ROLE':"
as_postgres -q -v ON_ERROR_STOP=1 -v role="$DB_ROLE" -v password="$DB_PASSWORD" <<'SQL'
SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'role') AS role_exists \gset
\if :role_exists
    ALTER ROLE :"role" WITH LOGIN PASSWORD :'password';
\else
    CREATE ROLE :"role" WITH LOGIN PASSWORD :'password';
\endif
SQL

# --- Database -------------------------------------------------------------
#
# Created if absent, then always (re)assigned to the role, so a database made by
# hand earlier ends up owned correctly rather than failing on the first
# migration.
if as_postgres -tAc "SELECT 1 FROM pg_database WHERE datname = '$DB_NAME'" | grep -q 1; then
    echo "database '$DB_NAME': already exists, leaving its data alone"
else
    as_postgres -q -c "CREATE DATABASE \"$DB_NAME\" OWNER \"$DB_ROLE\";"
    echo "database '$DB_NAME': created"
fi
as_postgres -q -c "ALTER DATABASE \"$DB_NAME\" OWNER TO \"$DB_ROLE\";"

# The role must own the public schema too. Since PostgreSQL 15 the public schema
# is owned by the database owner, so this is usually redundant — but on a
# database that was created by hand, and restored from elsewhere, it is not, and
# the failure it prevents is a migration that cannot create a table.
as_postgres -q -d "$DB_NAME" -c "ALTER SCHEMA public OWNER TO \"$DB_ROLE\";"

# --- Connection string ----------------------------------------------------
#
# `sslmode=disable` is correct here and worth understanding:
#
#   * The connection is to 127.0.0.1 on the same machine, so it does not cross a
#     network that anybody else can read.
#   * PostgreSQL's default sslmode is `prefer`, which means "use TLS if the
#     server offers it". On a stock Ubuntu cluster SSL is off, so `prefer`
#     quietly falls back to plaintext — which is fine, and is why this worked in
#     development.
#   * If somebody later enables SSL on the cluster with a self-signed
#     certificate, `prefer` would start negotiating TLS, and the app would then
#     fail on certificate verification. Saying `disable` explicitly makes that
#     impossible, and makes the intent legible rather than accidental.
#
# If the database ever moves to a different machine, change this to
# `sslmode=verify-full&sslrootcert=/path/to/ca.crt`. Do not use `require`:
# it encrypts but does not verify, which stops a passive eavesdropper and not an
# active one.

DB_URL="postgresql://${DB_ROLE}:${DB_PASSWORD}@${DB_HOST}:${DB_PORT}/${DB_NAME}?sslmode=disable"

# The URL is assembled from shell variables, so check it rather than trusting
# it. An empty password here produces a connection string the driver rejects at
# *startup* — long after this script said it was done — with a message about a
# missing password that says nothing about where the password should have come
# from. Failing here instead keeps the cause and the symptom in one place.
#
#   postgresql:// user : password @ host : port / db ? params
#                ^^^^   ^^^^^^^^
for part in "://" "@"; do
    case "$DB_URL" in
        *"$part"*) ;;
        *) die "assembled an unusable connection string (no '${part}'): check --db/--role/--host/--port" ;;
    esac
done
URL_USERINFO="${DB_URL#*://}"
URL_USERINFO="${URL_USERINFO%%@*}"
# `${x#*:}` strips the shortest prefix up to the first colon and leaves the
# rest. When there is no colon at all, it expands to `$x` unchanged, so that
# case has to be checked separately — testing only for non-emptiness would also
# reject a valid password that happens to be empty, which cannot happen here but
# would be the wrong reason to fail.
URL_PASSWORD="${URL_USERINFO#*:}"
[ "$URL_PASSWORD" != "$URL_USERINFO" ] || die "assembled a connection string with no password in it"
[ -n "$URL_PASSWORD" ] || die "assembled a connection string with an empty password"
[ -n "${URL_USERINFO%%:*}" ] || die "assembled a connection string with an empty user name"

# --- Write it out ---------------------------------------------------------
#
# The connection string carries the password. It goes in a 0600 file, owned by
# root on a real run, and the systemd unit reads it from there; the ownership
# matters because the unit runs as root and the *app* user has no business
# reading the file that contains the database password.
OUT_DIR="$(dirname "$OUT_FILE")"
# `install -d` on a path that already exists and is not ours can fail on an
# unusual filesystem; creating the directory without it is the fallback, so the
# script does not stop with a permissions complaint when it could carry on.
install -d -m 0750 "$OUT_DIR" 2>/dev/null || mkdir -p "$OUT_DIR"

# `umask 077` before creating, so the file is never briefly world-readable.
( umask 077; cat > "$OUT_FILE" <<EOF
# Written by deploy/provision-db.sh on $(date -u '+%Y-%m-%dT%H:%M:%SZ').
# Contains a database password. Keep it 0600 and out of version control.
CRM_DB=${DB_URL}
EOF
)
chmod 0600 "$OUT_FILE"
if [ "$(id -u)" -eq 0 ]; then
    chown root:root "$OUT_FILE"
else
    # Only reachable with CRM_PG_SUPERUSER set, i.e. in a test rather than a
    # deployment. Say so, because an env file owned by your login user is fine
    # for a local run and wrong for a service.
    echo "note: not running as root, so ${OUT_FILE} was left owned by $(id -un)" >&2
fi

# --- Verify ---------------------------------------------------------------
#
# Two separate questions, and an earlier version of this script conflated them:
#
#   1. Can a client reach the database as this role over TCP?
#   2. Is the password actually being checked when it does?
#
# Only the first is provable by connecting. On a cluster whose pg_hba.conf says
# `trust` — which is the Homebrew default, and a common development setup — the
# connection succeeds with *any* password, so a successful connect is no
# evidence at all that the credential is right. Asking pg_hba_file_rules what
# the matching rule actually says is what turns that into a real answer.
#
# Both column spellings are handled: PostgreSQL 14 calls these `type`,
# `database`, `user_name`; 15 renamed them to `rule_type`, `databases`,
# `user_names`. Matching either is cheaper than requiring a version.
echo
if PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_ROLE" -d "$DB_NAME" \
        -tAc "SELECT 'connected as ' || current_user || ' to ' || current_database();" 2>/dev/null; then
    AUTH_METHOD="$(as_postgres -tA -v ON_ERROR_STOP=1 -v db="$DB_NAME" -v role="$DB_ROLE" <<'SQL' 2>/dev/null || true
SELECT coalesce(
    (SELECT auth_method FROM pg_hba_file_rules
      WHERE type LIKE 'host%'
        AND ('all' = ANY(database) OR :'db' = ANY(database))
        AND ('all' = ANY(user_name) OR :'role' = ANY(user_name))
        AND error IS NULL
      ORDER BY line_number LIMIT 1),
    '') AS auth_method \gset
SELECT :'auth_method'
SQL
)"
    case "$AUTH_METHOD" in
        trust|peer|ident)
            cat >&2 <<EOF
WARNING: the connection works, but PostgreSQL is NOT checking the password.

  pg_hba.conf matches these connections with auth method '$AUTH_METHOD', so any
  password would have been accepted. The stored credential has been set
  correctly — this is about your server's configuration, not the script's — but
  it means the password is not protecting anything yet.

  For a database on the same machine this is a common and deliberate choice.
  If you want the password enforced, set the host line for 127.0.0.1/32 to
  'scram-sha-256' in $(as_postgres -tAc "SHOW hba_file" 2>/dev/null || echo pg_hba.conf),
  then: sudo systemctl reload postgresql
EOF
            ;;
        "")
            echo "note: could not read pg_hba.conf (permissions?); the password-"
            echo "      enforcement check was skipped, but the connection works."
            ;;
        *)
            echo "verified: connected as '$DB_ROLE' to '$DB_NAME', password enforced ('$AUTH_METHOD')."
            ;;
    esac
else
    die "created the role and database, but could not connect with them.
Check that PostgreSQL accepts host connections on ${DB_HOST}:${DB_PORT} —
in pg_hba.conf the line for host connections should say 'scram-sha-256' (or
'md5'), and a line above it saying 'reject' will win before yours is reached.
The connection string is in ${OUT_FILE}."
fi

cat <<EOF

Done.

  role        ${DB_ROLE}  (login, not superuser, no CREATEDB/CREATEROLE)
  database    ${DB_NAME}  (owned by ${DB_ROLE})
  env file    ${OUT_FILE}  (0600, owned by $(id -un))

The password is in ${OUT_FILE} and is not printed here on purpose, so it does not
end up in your shell history or a paste buffer.

Next: deploy/configure.sh (or follow deploy/README.md by hand) to generate the
app's cookie key and install the systemd unit.
EOF
