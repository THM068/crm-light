CREATE TABLE "companies" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "name" TEXT NOT NULL,
    "industry" TEXT,
    "website" TEXT,
    "phone" TEXT,
    "notes" TEXT,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_companies_by_name" ON "companies" ("name");
-- #[toasty::breakpoint]
CREATE TABLE "activities" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "kind" TEXT NOT NULL,
    "body" TEXT NOT NULL,
    "contact_id" INTEGER,
    "company_id" INTEGER,
    "deal_id" INTEGER,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_activities_by_kind" ON "activities" ("kind");
-- #[toasty::breakpoint]
CREATE INDEX "index_activities_by_contact_id" ON "activities" ("contact_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_activities_by_company_id" ON "activities" ("company_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_activities_by_deal_id" ON "activities" ("deal_id");
-- #[toasty::breakpoint]
CREATE TABLE "deals" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "title" TEXT NOT NULL,
    "value_cents" BIGINT NOT NULL,
    "stage" TEXT NOT NULL,
    "company_id" INTEGER,
    "contact_id" INTEGER,
    "expected_close" BIGINT,
    "notes" TEXT,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_deals_by_stage" ON "deals" ("stage");
-- #[toasty::breakpoint]
CREATE INDEX "index_deals_by_company_id" ON "deals" ("company_id");
-- #[toasty::breakpoint]
CREATE INDEX "index_deals_by_contact_id" ON "deals" ("contact_id");
-- #[toasty::breakpoint]
CREATE TABLE "contacts" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "first_name" TEXT NOT NULL,
    "last_name" TEXT NOT NULL,
    "email" TEXT,
    "phone" TEXT,
    "title" TEXT,
    "company_id" INTEGER,
    "notes" TEXT,
    "created_at" BIGINT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_contacts_by_email" ON "contacts" ("email");
-- #[toasty::breakpoint]
CREATE INDEX "index_contacts_by_company_id" ON "contacts" ("company_id");
