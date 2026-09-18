//! Toasty models — each struct maps to one SQLite table.
//!
//! Relationships are plain indexed foreign-key columns (`company_id`,
//! `contact_id`, `deal_id`) rather than `#[belongs_to]`/`#[has_many]`
//! relations, so every query in this app is explicit and easy to follow.

/// A customer account.
#[derive(Debug, toasty::Model)]
pub struct Company {
    #[key]
    #[auto]
    pub id: u64,

    #[index]
    pub name: String,

    pub industry: Option<String>,
    pub website: Option<String>,
    pub phone: Option<String>,
    pub notes: Option<String>,

    pub created_at: i64,
}

/// A person, optionally attached to a company.
#[derive(Debug, toasty::Model)]
pub struct Contact {
    #[key]
    #[auto]
    pub id: u64,

    pub first_name: String,
    pub last_name: String,

    #[index]
    pub email: Option<String>,
    pub phone: Option<String>,
    pub title: Option<String>,

    #[index]
    pub company_id: Option<u64>,

    pub notes: Option<String>,
    pub created_at: i64,
}

/// A sales opportunity.
#[derive(Debug, toasty::Model)]
pub struct Deal {
    #[key]
    #[auto]
    pub id: u64,

    pub title: String,

    /// Value in cents; see [`crate::domain::format_money`].
    pub value_cents: i64,

    #[index]
    pub stage: String,

    #[index]
    pub company_id: Option<u64>,

    #[index]
    pub contact_id: Option<u64>,

    /// Expected close date as Unix seconds, normalised to midnight UTC.
    pub expected_close: Option<i64>,

    pub notes: Option<String>,
    pub created_at: i64,
}

/// A logged interaction, optionally attached to a contact, company, or deal.
#[derive(Debug, toasty::Model)]
pub struct Activity {
    #[key]
    #[auto]
    pub id: u64,

    /// One of `note`, `call`, `email`, `meeting`.
    #[index]
    pub kind: String,

    pub body: String,

    #[index]
    pub contact_id: Option<u64>,

    #[index]
    pub company_id: Option<u64>,

    #[index]
    pub deal_id: Option<u64>,

    pub created_at: i64,
}
