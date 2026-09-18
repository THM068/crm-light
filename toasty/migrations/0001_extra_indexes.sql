-- Indexes the generated migration cannot express.
--
-- Toasty's `#[index]` creates a single-column index per field, and its root
-- models have no equivalent of a composite index. Two places need one:
--
-- 1. The list pages all sort by `(sort column, id)`, because the id is what
--    makes the order total: without it, two rows sharing a sort key have no
--    defined order between them and a row can appear on two pages or none.
--    With separate single-column indexes the database can either seek the sort
--    column and sort the ties itself or seek the primary key and filter — it
--    cannot do both, so every page becomes a sort of an ever-larger prefix of
--    the table.
--
-- 2. Every query filters on `account_id` first and then looks something up
--    inside that workspace, so the tenant column leads the index it is used
--    with. `deals` and `activities` are also sorted by primary key, which the
--    existing primary-key index covers.
--
-- PostgreSQL is the only supported backend (see the README), so the SQL is
-- written for it.
--
-- `CREATE INDEX` takes a lock that blocks writes for the duration. On a table
-- that already has millions of rows, run these by hand with
-- `CREATE INDEX CONCURRENTLY` instead, outside a transaction.

-- Sort orders used by the list pages.
CREATE INDEX "index_companies_by_name_id" ON "companies" ("name", "id");
CREATE INDEX "index_contacts_by_last_name_id" ON "contacts" ("last_name", "id");
CREATE INDEX "index_users_by_account_username" ON "users" ("account_id", "username_lower");
CREATE INDEX "index_login_attempts_by_account_username"
    ON "login_attempts" ("account_id", "username_lower");

-- Tenant-first lookups: the account filter is on every one of these queries,
-- so it belongs at the front of the index rather than letting the planner
-- choose between the tenant index and the foreign-key index and then filter.
CREATE INDEX "index_deals_by_account_id_id" ON "deals" ("account_id", "id" DESC);
CREATE INDEX "index_activities_by_account_id_id" ON "activities" ("account_id", "id" DESC);
CREATE INDEX "index_companies_by_account_name" ON "companies" ("account_id", "name");
CREATE INDEX "index_contacts_by_account_last_name" ON "contacts" ("account_id", "last_name");
CREATE INDEX "index_sessions_by_account_token" ON "sessions" ("account_id", "token_hash");
