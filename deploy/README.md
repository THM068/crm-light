# Deploying crm-light on an Ubuntu server

Everything here runs **on the server**, not on your laptop. This directory
contains the scripts, the systemd unit, and an nginx starting point; this file
is the order to run them in and the reasoning you need when something does not
work.

The shape being built:

```
   browser ── https ──▶ nginx :443 ── http ──▶ crm-light 127.0.0.1:3000
                                                    │
                                                    └─▶ PostgreSQL 127.0.0.1:5432
```

Only nginx is reachable from outside. The app listens on loopback and talks to
PostgreSQL over loopback, so TLS terminates in one place and neither the app nor
the database is exposed directly.

---

## 1. Packages

```bash
sudo apt update
sudo apt install -y postgresql nginx certbot python3-certbot-nginx openssl curl
```

If you are going to build on the server rather than cross-compile:

```bash
sudo apt install -y build-essential pkg-config libssl-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 2. Get the code onto the server

```bash
sudo mkdir -p /srv && cd /srv
git clone <your-repo> crm-light
cd crm-light
```

## 3. Create the database and its credentials

```bash
sudo ./deploy/provision-db.sh
```

That creates a **non-superuser role** and a database owned by it, generates a
32-character alphanumeric password, verifies the credentials actually
authenticate over TCP, and writes the connection string to
`/etc/crm-light/db.env` (mode `0600`, root-only).

The password is deliberately **not printed**. It is in that file — read it with
`sudo cat /etc/crm-light/db.env` if you want it, but the app reads it from there,
so you rarely need to.

Useful variations:

```bash
# Different names
sudo ./deploy/provision-db.sh --db crm_prod --role crm_app

# Rotate the password without touching the data
sudo ./deploy/provision-db.sh

# Start over, DESTROYING everything in the database
sudo ./deploy/provision-db.sh --drop
```

Check it worked:

```bash
sudo -u postgres psql -c "\du crm_light"
sudo -u postgres psql -d crm_light -c "\dt"    # empty until the app first runs
```

## 4. Build the binary

```bash
cargo build --release
```

Roughly a minute and a few hundred MB of disk for `target/`. If the server is
small, build elsewhere and copy `target/release/crm-light` over instead.

## 5. Generate the app's secrets and install the service

```bash
sudo ./deploy/configure.sh --domain crm.example.com --user crm-light
```

That creates a system user with no shell, generates `CRM_SECRET_KEY` (64 bytes;
it signs and encrypts cookies, so it is generated once and preserved on re-runs
— regenerating it would sign everybody out), writes `/etc/crm-light/app.env`,
and installs and enables the unit.

Options worth knowing:

| Flag | Why you would use it |
| ---- | -------------------- |
| `--allow-signup` | While creating your first workspace. See step 7. |
| `--no-tls` | Only if you are serving plain HTTP on a private network. Without this, cookies carry `Secure`, browsers will not send them over HTTP, and sign-in fails with no visible error. |
| `--port 8080` | If something else already holds 3000. |

## 6. Install the binary and start it

```bash
sudo install -o crm-light -g crm-light -m 0755 \
    target/release/crm-light /opt/crm-light/crm-light
sudo systemctl enable --now crm-light
sudo systemctl status crm-light
```

On first start it applies its migrations and prints where to sign up:

```
crm-light: PostgreSQL at postgresql://127.0.0.1:5432/crm_light, times shown in UTC
sign up at http://127.0.0.1:3000/signup to create the first workspace
applied 2 migration(s)
```

The unit's hardening means the app can only write to its own temporary
directory, and `MemoryMax=512M` caps it. If a future change needs to write a
file somewhere, `ProtectSystem=strict` is what will stop it — that is the
setting to relax, deliberately.

## 7. Create your workspace, then close sign-up

This is the step that is easy to forget and matters most on a public server.
While `CRM_ALLOW_SIGNUP` is on, **anyone who can reach the port can create a
workspace of their own.**

```bash
# Temporarily allow sign-up
sudo ./deploy/configure.sh --domain crm.example.com --allow-signup
sudo systemctl restart crm-light

# Visit https://crm.example.com/signup, create the workspace, sign in.
# Then close it again:
sudo ./deploy/configure.sh --domain crm.example.com
sudo systemctl restart crm-light
```

With it off, `/signup` returns a 404 and the login page says the installation is
not accepting new workspaces. Existing workspaces are unaffected, and
administrators keep adding people at `/admin/users`.

## 8. Put TLS in front of it

```bash
sudo cp deploy/nginx/crm-light.conf /etc/nginx/sites-available/crm-light
sudo sed -i 's/crm\.example\.com/your.actual.domain/g' /etc/nginx/sites-available/crm-light
sudo ln -s /etc/nginx/sites-available/crm-light /etc/nginx/sites-enabled/crm-light
sudo rm -f /etc/nginx/sites-enabled/default
sudo nginx -t && sudo systemctl reload nginx
sudo certbot --nginx -d your.actual.domain
```

`certbot` installs the certificate and a renewal timer. Confirm renewal works
before you rely on it:

```bash
sudo certbot renew --dry-run
```

Once you are confident, uncomment the HSTS line in the nginx config. Not before:
browsers cache it, and if the certificate then fails to renew, the site is
unreachable until it expires.

## 9. Check it end to end

```bash
# The app is up and only on loopback
sudo ss -ltnp | grep -E ':(3000|5432|443|80)\b'

# It answers, and refuses anonymous requests
curl -sI https://your.actual.domain/ | head -3      # expect 307 to /login
curl -sI https://your.actual.domain/login | head -1 # expect 200

# The cookie is Secure over TLS
curl -sI https://your.actual.domain/login | grep -i set-cookie
```

Then sign in in a browser and confirm the stylesheet loads — a deployment where
the app answers but looks unstyled usually means a proxy is intercepting
`/style.css`.

---

## Operating it

**Logs.** `journalctl -u crm-light -f`. The app logs to stdout and stderr only;
nginx has its own logs under `/var/log/nginx/`.

**Backups.** The database is the whole application state:

```bash
sudo -u postgres pg_dump -Fc crm_light | gzip > crm-light-$(date +%F).dump.gz
```

Restore with `pg_restore`. Back up `/etc/crm-light/app.env` too — losing
`CRM_SECRET_KEY` signs everyone out, and losing `db.env` loses the password.

**Upgrades.**

```bash
cd /srv/crm-light && git pull
cargo build --release
sudo install -o crm-light -g crm-light -m 0755 target/release/crm-light /opt/crm-light/crm-light
sudo systemctl restart crm-light
```

Migrations are applied at startup and are idempotent, so a restart is the
upgrade. That also means **a restart is a schema change**: take a backup before
deploying one, because rolling back the binary does not roll back the schema.

**Rotating the database password.**

```bash
sudo ./deploy/provision-db.sh                     # new password, same data
sudo ./deploy/configure.sh --domain your.domain   # re-reads db.env
sudo systemctl restart crm-light
```

**Rotating the cookie key.** Edit `CRM_SECRET_KEY` in
`/etc/crm-light/app.env` and restart. Everybody is signed out; nothing else
breaks.

---

## Environment variables

`deploy/configure.sh` writes sane values for all of these. The full table is in
the main `README.md`; these are the ones that matter for a server, and the
defaults that are wrong for one:

| Variable | Set it to | Because |
| -------- | --------- | ------- |
| `CRM_DB` | the generated URL | Written by `provision-db.sh`. |
| `CRM_SECRET_KEY` | 64 random bytes | Without it a key is generated per process and **every restart signs everyone out**. |
| `CRM_COOKIE_SECURE` | `1` behind TLS, `0` without | Wrong either way it breaks sign-in silently. |
| `CRM_ALLOW_SIGNUP` | `0` | Otherwise the internet can create workspaces. |
| `CRM_SEED` | `0` | Otherwise the demo companies are inserted into your real installation. |
| `HOST` | `127.0.0.1` | Otherwise the app is reachable around the proxy, without TLS. |
| `CRM_TZ` | your zone | So dates read the way your team writes them. Storage stays UTC. |

---

## Troubleshooting

**`systemctl status` shows the app restarting in a loop.** `journalctl -u crm-light -n 50`.
The two usual causes are a bad `CRM_DB` and a `CRM_SECRET_KEY` that is not at
least 64 bytes — the app refuses to start rather than running with a key that
signs cookies weakly, and says so.

**`connection refused` to PostgreSQL.** `sudo systemctl status postgresql`. On a
stock Ubuntu install it listens on `127.0.0.1:5432`; if it is on a unix socket
only, check `listen_addresses` in `postgresql.conf`.

**`password authentication failed`.** The password in `db.env` and the one on
the role have diverged — usually because `provision-db.sh` was run but the app
was not restarted, or because `app.env` still holds an old `CRM_DB`. Re-run
`configure.sh` and restart. If you edited `pg_hba.conf`, remember that the first
matching line wins: a `trust` line above your `scram-sha-256` line means the
password is never checked.

**Sign-in appears to do nothing.** Almost always `CRM_COOKIE_SECURE=1` while
serving plain HTTP: the browser receives the cookie and refuses to send it back.
Set it to `0` or put TLS in front.

**Pages load but look unstyled.** Something between the browser and the app is
answering for `/style.css`. Check `curl -sI https://your.domain/style.css`
returns `200` and `content-type: text/css`.

**A workspace exists but nobody can sign in to it.** Sign-in needs the workspace
slug, which `/signup` derived from the name you typed. Find it with:

```bash
sudo -u postgres psql -d crm_light -c "SELECT id, name, slug FROM accounts;"
```

---

## What this deployment does not do

Worth knowing before you rely on it:

- **No automatic backups.** The `pg_dump` above is a command, not a schedule.
- **No monitoring or alerting.** A crashed app is restarted by systemd; an app
  that is up but failing is silent.
- **No log rotation for the app.** `journald` handles it by size; tune
  `SystemMaxUse` in `/etc/systemd/journald.conf` if the disk is small.
- **No superuser.** There is no way to see all workspaces or rescue one that has
  lost its last administrator without editing the database by hand. See the main
  README's limitations.
- **Sessions live in the database**, so a restart does not sign anyone out —
  provided `CRM_SECRET_KEY` is set and preserved.
