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

[ "$(id -u)" -eq 0 ] || die "run this with sudo (it needs to act as the postgres user)"
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
as_postgres() {
    sudo -u postgres env -u PGHOST -u PGPORT -u PGDATABASE -u PGUSER psql -v ON_ERROR_STOP=1 "$@"
}

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
# `\gexec` runs whatever the query returns, which is how the create and the
# rotate can be one code path: the query either emits a CREATE ROLE or an ALTER
# ROLE, and neither the password nor the SQL is ever on a command line.
echo "role '$DB_ROLE':"
as_postgres -q <<SQL
SELECT format(
    CASE WHEN EXISTS (SELECT 1 FROM pg_roles WHERE rolname = %L)
         THEN 'ALTER ROLE %I WITH LOGIN PASSWORD %L'
         ELSE 'CREATE ROLE %I WITH LOGIN PASSWORD %L'
    END,
    '$DB_ROLE', '$DB_ROLE', '$DB_PASSWORD')
\gexec
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

# --- Write it out ---------------------------------------------------------
#
# The connection string carries the password. It goes in a 0600 file owned by
# root, and the systemd unit reads it from there; see deploy/systemd/README.md
# for why it is not simply an Environment= line in the unit.
OUT_DIR="$(dirname "$OUT_FILE")"
install -d -m 0750 "$OUT_DIR"

# `umask 077` before creating, so the file is never briefly world-readable.
( umask 077; cat > "$OUT_FILE" <<EOF
# Written by deploy/provision-db.sh on $(date -u '+%Y-%m-%dT%H:%M:%SZ').
# Contains a database password. Keep it 0600 and out of version control.
CRM_DB=${DB_URL}
EOF
)
chmod 0600 "$OUT_FILE"
chown root:root "$OUT_FILE"

# --- Verify ---------------------------------------------------------------
#
# Prove the credentials work before claiming success. `\conninfo` is not enough;
# this actually authenticates over TCP with the password, which is exactly what
# the app will do at startup.
if PGPASSWORD="$DB_PASSWORD" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_ROLE" -d "$DB_NAME" \
        -tAc "SELECT 'connected as ' || current_user || ' to ' || current_database();" 2>/dev/null; then
    :
else
    die "created the role and database, but could not connect with them.
Check that PostgreSQL accepts password authentication on ${DB_HOST}:${DB_PORT} —
in pg_hba.conf the line for host connections should say 'scram-sha-256' (or
'md5'), and 'trust' or 'peer' where you expected a password usually means the
line above it matched first.
The connection string is in ${OUT_FILE}."
fi

cat <<EOF

Done.

  role        ${DB_ROLE}  (login, not superuser, no CREATEDB/CREATEROLE)
  database    ${DB_NAME}  (owned by ${DB_ROLE})
  env file    ${OUT_FILE}  (0600, root:root)

The password is in ${OUT_FILE} and is not printed here on purpose, so it does not
end up in your shell history or a paste buffer.

Next: deploy/configure.sh (or follow deploy/README.md by hand) to generate the
app's cookie key and install the systemd unit.
EOF
