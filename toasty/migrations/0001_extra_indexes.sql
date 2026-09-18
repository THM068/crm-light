-- Indexes the generated migration cannot express.
--
-- Toasty's `#[index]` creates a single-column index per field, and its root
-- models have no equivalent of a composite index. The list pages all sort by
-- `(sort column, id)`, because the id is what makes the order total: without
-- it, two rows sharing a sort key have no defined order between them, and a row
-- can then appear on two pages or on none.
--
-- With separate single-column indexes, the database can either seek the sort
-- column and then sort the ties itself, or seek the primary key and filter — it
-- cannot do both at once, so every page becomes a sort of an ever-larger prefix
-- of the table. These indexes let it walk the exact order the page asks for.
--
-- `deals` and `activities` need nothing extra: their sort key *is* the primary
-- key, so the existing primary-key index already serves them.
--
-- PostgreSQL is the only supported backend (see the README), so the SQL is
-- written for it.
--
-- `CREATE INDEX` takes a lock that blocks writes for the duration. On a table
-- that already has millions of rows, run these by hand with
-- `CREATE INDEX CONCURRENTLY` instead, outside a transaction.

CREATE INDEX "index_companies_by_name_id" ON "companies" ("name", "id");
CREATE INDEX "index_contacts_by_last_name_id" ON "contacts" ("last_name", "id");
CREATE INDEX "index_users_by_username_lower_id" ON "users" ("username_lower", "id");
