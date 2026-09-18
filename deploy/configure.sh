#!/usr/bin/env bash
#
# Generate the application's secrets, write its environment file, and install
# the systemd unit that runs it.
#
# Run it on the server, after deploy/provision-db.sh:
#
#     sudo ./deploy/configure.sh --domain crm.example.com --user deploy
#
# Idempotent in the way that matters: the cookie key and the database password
# are only generated if they do not already exist. Re-running does not sign
# everybody out, and does not touch the database.
#
# What it decides, and why:
#
#   * CRM_SECRET_KEY is 64 bytes of base64. It signs and encrypts cookies, so
#     regenerating it invalidates every session. That is why it is generated
#     once and then preserved.
#
#   * CRM_COOKIE_SECURE=1, because the unit is intended to sit behind a TLS
#     proxy. If you are serving plain HTTP on a private network, pass
#     --no-tls, or the browser will refuse to send the session cookie and nobody
#     will be able to sign in.
#
#   * CRM_ALLOW_SIGNUP=0 by default. An internet-reachable installation should
#     not let strangers create workspaces. Sign up first, then run this, or run
#     it with --allow-signup while you are getting set up.
#
#   * CRM_SEED=0, because demo companies in a real installation are noise.
#
#   * CRM_TZ is left to the machine, with a note: set it explicitly if the
#     server's clock is not in the zone your team reads dates in.
#
#   * HOST=127.0.0.1, so the app is only reachable through the reverse proxy.
#     Exposing it directly as well would let somebody bypass the proxy, and with
#     it TLS and any rate limiting you have put in front.

set -euo pipefail

APP_USER="crm-light"
APP_DIR="/opt/crm-light"
ENV_FILE="/etc/crm-light/app.env"
SERVICE_NAME="crm-light"
DB_ENV_FILE="/etc/crm-light/db.env"
PORT="3000"
DOMAIN=""
USE_TLS="yes"
ALLOW_SIGNUP="no"
SETUP_USER=""

usage() {
    cat <<'USAGE'
Usage: sudo ./deploy/configure.sh [options]

  --domain NAME        hostname the app will be served on (used by the nginx
                       example and printed in the summary)
  --user NAME          system user to run the service as   (default: crm-light)
  --dir PATH           where the binary lives              (default: /opt/crm-light)
  --port PORT          port the app listens on, locally    (default: 3000)
  --env PATH           where to write the env file         (default: /etc/crm-light/app.env)
  --db-env PATH        the file provision-db.sh wrote      (default: /etc/crm-light/db.env)
  --allow-signup       keep public sign-up enabled (° see below)
  --no-tls             the app is served over plain HTTP; drops the Secure flag
                       from cookies so a browser will still send them
  -h, --help           this message

° An internet-reachable installation should keep --allow-signup off. Turn it on
  only while creating your first workspace, then run this again without it.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --domain) DOMAIN="$2"; shift 2 ;;
        --user) APP_USER="$2"; shift 2 ;;
        --dir) APP_DIR="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        --env) ENV_FILE="$2"; shift 2 ;;
        --db-env) DB_ENV_FILE="$2"; shift 2 ;;
        --allow-signup) ALLOW_SIGNUP="yes"; shift ;;
        --no-tls) USE_TLS="no"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

die() { echo "configure: $*" >&2; exit 1; }

# CRM_CONFIGURE_TESTING skips everything that needs root and systemd — creating
# the user, installing the unit, reloading the daemon — while still doing the
# validation and the env file. It exists so this script can be exercised without
# a server; it is not a deployment mode, and it says so when used.
TESTING="no"
if [ -n "${CRM_CONFIGURE_TESTING:-}" ]; then
    TESTING="yes"
    echo "note: CRM_CONFIGURE_TESTING is set — validating and writing files only;" >&2
    echo "      no user, unit, or daemon-reload. Not a deployment." >&2
else
    [ "$(id -u)" -eq 0 ] || die "run this with sudo (it creates a system user and a unit)"
    command -v systemctl >/dev/null 2>&1 || die "systemd not found; this script installs a systemd unit"
fi
[ -f "$DB_ENV_FILE" ] || die "$DB_ENV_FILE not found. Run deploy/provision-db.sh first."

# Read CRM_DB out of the file provision-db.sh wrote, rather than asking for it
# again: one source of truth, and the password never has to be pasted anywhere.
DB_URL="$(grep -E '^CRM_DB=' "$DB_ENV_FILE" | head -1 | cut -d= -f2-)"
[ -n "$DB_URL" ] || die "no CRM_DB= line in $DB_ENV_FILE"

# Check the URL before copying it into the app's environment. Without this, a
# db.env that is missing its password — hand-edited, or written by an earlier
# version of provision-db.sh — is copied straight through, and the app fails at
# startup with "password missing", which points at the app rather than at the
# file that is actually wrong.
case "$DB_URL" in
    *"://"*"@"*) ;;
    *) die "$DB_ENV_FILE has a CRM_DB that is not a usable connection URL.
It should look like:
  postgresql://USER:PASSWORD@127.0.0.1:5432/DBNAME?sslmode=disable
Found:
  ${DB_URL}" ;;
esac
URL_USERINFO="${DB_URL#*://}"
URL_USERINFO="${URL_USERINFO%%@*}"
URL_PASSWORD="${URL_USERINFO#*:}"
if [ "$URL_PASSWORD" = "$URL_USERINFO" ] || [ -z "$URL_PASSWORD" ]; then
    die "$DB_ENV_FILE has a CRM_DB with no password in it:
  ${DB_URL}
Re-run the database step, which regenerates it:
  sudo ./deploy/provision-db.sh"
fi
# Printed redacted, so the operator can see the shape without the secret.
DB_URL_REDACTED="$(printf '%s' "$DB_URL" | sed 's#://[^@]*@#://***@#')"

ENV_DIR="$(dirname "$ENV_FILE")"
# `install -d` cannot set a mode on a path it does not own on some filesystems;
# the mkdir fallback keeps the script going where the mode is already fine.
install -d -m 0750 "$ENV_DIR" 2>/dev/null || mkdir -p "$ENV_DIR"
SERVICE_DIR="/etc/systemd/system"

# --- The service user -----------------------------------------------------
#
# A system account with no shell and no home. The app needs no privileges: it
# listens on a high port and writes to the database, and nothing else. Running
# it as your login user would mean a bug in it is a bug with your ssh keys.
if [ "$TESTING" = "yes" ]; then
    echo "user '$APP_USER': skipped (testing)"
elif id "$APP_USER" >/dev/null 2>&1; then
    echo "user '$APP_USER': already exists"
else
    useradd --system --no-create-home --shell /usr/sbin/nologin "$APP_USER"
    echo "user '$APP_USER': created (system account, no shell)"
fi

if [ "$TESTING" = "yes" ]; then
    mkdir -p "$APP_DIR"
else
    install -d -m 0755 -o "$APP_USER" -g "$APP_USER" "$APP_DIR"
fi

# --- Cookie key -----------------------------------------------------------
#
# Preserved if present. Regenerating it would sign every user out and invalidate
# every CSRF token, which is a surprising thing for a re-run to do.
if [ -f "$ENV_FILE" ] && grep -q '^CRM_SECRET_KEY=' "$ENV_FILE"; then
    SECRET_KEY="$(grep '^CRM_SECRET_KEY=' "$ENV_FILE" | head -1 | cut -d= -f2-)"
    echo "CRM_SECRET_KEY: reusing the existing one (sessions stay valid)"
else
    SECRET_KEY="$(openssl rand -base64 64 | tr -d '\n')"
    echo "CRM_SECRET_KEY: generated (64 bytes)"
fi

if [ "$USE_TLS" = "yes" ]; then
    COOKIE_SECURE="1"
else
    COOKIE_SECURE="0"
fi

# The env file speaks 1/0 throughout, which is what the app parses. A generated
# file that mixes `yes` and `1` invites somebody to copy the wrong style into an
# edit, so pick one and use it for every flag.
if [ "$ALLOW_SIGNUP" = "yes" ]; then
    ALLOW_SIGNUP_FLAG="1"
else
    ALLOW_SIGNUP_FLAG="0"
fi

# --- Environment file -----------------------------------------------------
#
# `umask 077` so the secrets are never briefly world-readable, and 0640 with the
# service's group so systemd — running as root — can read it while the app user
# cannot be bothered to.
( umask 077; cat > "$ENV_FILE" <<EOF
# Written by deploy/configure.sh on $(date -u '+%Y-%m-%dT%H:%M:%SZ').
# Contains secrets. Keep it 0600 and out of version control.

# --- Where the data lives -------------------------------------------------
# Generated by deploy/provision-db.sh; see that file for the sslmode reasoning.
CRM_DB=${DB_URL}

# --- Cookies and sessions -------------------------------------------------
# Signs and encrypts cookies. Changing it signs everybody out.
CRM_SECRET_KEY=${SECRET_KEY}
# Set when cookies must travel over TLS. Must be 0 if you serve plain HTTP, or
# browsers will not send the session cookie back and sign-in will silently fail.
CRM_COOKIE_SECURE=${COOKIE_SECURE}
CRM_SESSION_TTL_HOURS=336

# --- Who can get in -------------------------------------------------------
# Turn off once your workspaces exist: with this on, anyone who can reach the
# port can create a tenant of their own.
CRM_ALLOW_SIGNUP=${ALLOW_SIGNUP_FLAG}
# No account is created without a password, so this only affects rows left by an
# older version. Off is the safe setting.
CRM_ALLOW_EMPTY_PASSWORD=0
CRM_LOGIN_MAX_ATTEMPTS=8
CRM_LOGIN_LOCKOUT_MINUTES=15

# --- Presentation ---------------------------------------------------------
# Left unset so the host's zone is used. Set it explicitly if the server's clock
# is not in the zone your team reads dates in (it is a *display* setting: storage
# is always UTC).
#CRM_TZ=Europe/London
CRM_PAGE_SIZE=25

# --- First-run behaviour --------------------------------------------------
# Demo companies in a real installation are noise.
CRM_SEED=0

# --- Where it listens -----------------------------------------------------
# Loopback only. The reverse proxy is the way in, and it is where TLS and any
# rate limiting live; binding 0.0.0.0 as well would let somebody step around it.
HOST=127.0.0.1
PORT=${PORT}
EOF
)
chmod 0640 "$ENV_FILE"
if [ "$TESTING" = "yes" ]; then
    echo "env file: $ENV_FILE (0640, owned by $(id -un))"
else
    chown "root:${APP_USER}" "$ENV_FILE"
    echo "env file: $ENV_FILE (0640, root:$APP_USER)"
fi
echo "  CRM_DB: ${DB_URL_REDACTED}"

if [ "$TESTING" = "yes" ]; then
    echo "unit: skipped (testing)"
    echo
    echo "Done (testing). Wrote $ENV_FILE; no unit installed."
    exit 0
fi

# --- systemd unit ---------------------------------------------------------
#
# The unit reads the env file rather than inlining the variables, so
# `systemctl show crm-light` does not spill the database password to anyone who
# can run it, and so a rotation is an edit to one file rather than
# `systemctl edit`.
cat > "${SERVICE_DIR}/${SERVICE_NAME}.service" <<EOF
[Unit]
Description=crm-light
Documentation=file://${APP_DIR}/README.md
After=network-online.target postgresql.service
Wants=network-online.target
Requires=postgresql.service

[Service]
Type=simple
User=${APP_USER}
Group=${APP_USER}
WorkingDirectory=${APP_DIR}
EnvironmentFile=${ENV_FILE}
ExecStart=${APP_DIR}/crm-light
Restart=on-failure
RestartSec=2s

# The app holds no files and needs no privileges. Everything below is defence in
# depth for a process that is reachable from the network.
NoNewPrivileges=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectSystem=strict
ProtectHome=yes
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
SystemCallArchitectures=native
# Needs to open a TCP socket to PostgreSQL on loopback, and to listen on it.
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX

# A runaway process should be restarted, not left holding memory.
MemoryMax=512M

[Install]
WantedBy=multi-user.target
EOF
echo "unit: ${SERVICE_DIR}/${SERVICE_NAME}.service"

systemctl daemon-reload
echo
cat <<EOF
Done. The unit is installed but not started, because the binary may not be in
place yet.

  1. Build on the server (or copy the binary in):

         cargo build --release
         sudo install -o ${APP_USER} -g ${APP_USER} -m 0755 \\
             target/release/crm-light ${APP_DIR}/crm-light

  2. Start it:

         sudo systemctl enable --now ${SERVICE_NAME}
         sudo systemctl status ${SERVICE_NAME}
         journalctl -u ${SERVICE_NAME} -f

  3. Give it a TLS front door. deploy/nginx/crm-light.conf is a starting point:

         sudo cp deploy/nginx/crm-light.conf /etc/nginx/sites-available/${SERVICE_NAME}
         sudo ln -s /etc/nginx/sites-available/${SERVICE_NAME} /etc/nginx/sites-enabled/
         sudo nginx -t && sudo systemctl reload nginx
         sudo certbot --nginx -d ${DOMAIN:-crm.example.com}
EOF

if [ "$ALLOW_SIGNUP" = "yes" ]; then
    cat <<EOF

  NOTE: public sign-up is ON, so anyone who can reach this host can create a
  workspace. Create yours at /signup, then turn it off:

        sudo ./deploy/configure.sh${DOMAIN:+ --domain $DOMAIN}
        sudo systemctl restart ${SERVICE_NAME}
EOF
fi
