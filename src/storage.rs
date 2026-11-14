//! SQLite persistence for Pockets.
//!
//! Business rules live in `domain`. This module loads records, calls the domain
//! functions, and writes results. Multi-row changes run in a single transaction.

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::domain::{
    self, BillingType, DomainError, Id, InvoiceLine, InvoiceStatus, LineSource, Milestone, Project,
    ProjectStatus, TimeEntry,
};

#[derive(Debug)]
pub enum StorageError {
    Sql(rusqlite::Error),
    Domain(DomainError),
    NotFound {
        kind: &'static str,
        id: Id,
    },
    /// A write matched a different number of rows than expected, which means the
    /// record changed between read and write.
    Conflict(&'static str),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(e) => write!(f, "database error: {e}"),
            Self::Domain(e) => write!(f, "{e}"),
            Self::NotFound { kind, id } => write!(f, "{kind} {id} not found"),
            Self::Conflict(what) => write!(f, "{what} changed while saving; please reload"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<rusqlite::Error> for StorageError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

impl From<DomainError> for StorageError {
    fn from(e: DomainError) -> Self {
        Self::Domain(e)
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub id: Id,
    pub name: String,
    pub contact: String,
    pub address: String,
    pub payment_terms_days: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invoice {
    pub id: Id,
    pub client_id: Id,
    pub project_id: Id,
    pub number: Option<String>,
    pub status: InvoiceStatus,
    /// ISO date, `YYYY-MM-DD`.
    pub issue_date: String,
    /// ISO date, `YYYY-MM-DD`.
    pub due_date: String,
    pub total_cents: i64,
    /// Currency captured when the invoice was drafted.
    pub currency: String,
}

/// Complete invoice data for review and export. Business details and currency are
/// read from the snapshot stored when the invoice was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceDocument {
    pub invoice: Invoice,
    pub client: Client,
    pub project: Project,
    pub currency: String,
    pub business_name: String,
    pub business_address: String,
    pub business_contact: String,
    pub lines: Vec<InvoiceLine>,
}

/// Current Unix time in seconds.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

const MIGRATIONS: &[&str] = &[
    // v1: initial schema.
    "
    CREATE TABLE clients (
        id          INTEGER PRIMARY KEY,
        name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
        contact     TEXT NOT NULL DEFAULT '',
        address     TEXT NOT NULL DEFAULT '',
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE projects (
        id                INTEGER PRIMARY KEY,
        client_id         INTEGER NOT NULL REFERENCES clients(id),
        name              TEXT NOT NULL CHECK (length(trim(name)) > 0),
        billing           TEXT NOT NULL CHECK (billing IN ('hourly', 'fixed_price')),
        hourly_rate_cents INTEGER NOT NULL DEFAULT 0 CHECK (hourly_rate_cents >= 0),
        status            TEXT NOT NULL CHECK (status IN ('active', 'completed')),
        created_at        INTEGER NOT NULL,
        updated_at        INTEGER NOT NULL
    );

    CREATE TABLE invoices (
        id          INTEGER PRIMARY KEY,
        client_id   INTEGER NOT NULL REFERENCES clients(id),
        project_id  INTEGER NOT NULL REFERENCES projects(id),
        number      TEXT UNIQUE,
        status      TEXT NOT NULL CHECK (status IN ('draft', 'sent', 'paid')),
        issue_date  TEXT NOT NULL CHECK (issue_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
        due_date    TEXT NOT NULL CHECK (due_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
        total_cents INTEGER NOT NULL CHECK (total_cents >= 0),
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        CHECK (due_date >= issue_date)
    );

    CREATE TABLE time_entries (
        id          INTEGER PRIMARY KEY,
        project_id  INTEGER NOT NULL REFERENCES projects(id),
        description TEXT NOT NULL DEFAULT '',
        start_at    INTEGER NOT NULL,
        end_at      INTEGER,
        billable    INTEGER NOT NULL DEFAULT 1 CHECK (billable IN (0, 1)),
        invoice_id  INTEGER REFERENCES invoices(id),
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL,
        CHECK (end_at IS NULL OR end_at >= start_at)
    );

    -- At most one running timer across all projects.
    CREATE UNIQUE INDEX one_running_timer
        ON time_entries ((end_at IS NULL))
        WHERE end_at IS NULL;

    CREATE TABLE milestones (
        id           INTEGER PRIMARY KEY,
        project_id   INTEGER NOT NULL REFERENCES projects(id),
        name         TEXT NOT NULL CHECK (length(trim(name)) > 0),
        amount_cents INTEGER NOT NULL CHECK (amount_cents >= 0),
        invoice_id   INTEGER REFERENCES invoices(id),
        created_at   INTEGER NOT NULL,
        updated_at   INTEGER NOT NULL
    );

    CREATE TABLE invoice_lines (
        id               INTEGER PRIMARY KEY,
        invoice_id       INTEGER NOT NULL REFERENCES invoices(id),
        description      TEXT NOT NULL,
        quantity_seconds INTEGER,
        amount_cents     INTEGER NOT NULL CHECK (amount_cents >= 0),
        source_kind      TEXT NOT NULL CHECK (source_kind IN ('time_entry', 'milestone')),
        source_id        INTEGER NOT NULL
    );

    CREATE INDEX invoice_lines_by_invoice ON invoice_lines (invoice_id);
    CREATE INDEX time_entries_by_project ON time_entries (project_id);
    CREATE INDEX milestones_by_project ON milestones (project_id);
    ",
    // v2: milestones must be marked delivered before they can be invoiced.
    // Existing rows start undelivered.
    "
    ALTER TABLE milestones ADD COLUMN delivered_at INTEGER;
    ",
    // v3: payment terms on clients, invoice snapshots taken at creation and send,
    // and a settings table that holds the invoice number sequence.
    // Defaults for terms (30 days), currency (USD), and the prefix (INV) are
    // placeholders until the owner confirms them.
    "
    ALTER TABLE clients ADD COLUMN payment_terms_days INTEGER NOT NULL DEFAULT 30
        CHECK (payment_terms_days >= 0);
    ALTER TABLE invoices ADD COLUMN terms_days INTEGER CHECK (terms_days IS NULL OR terms_days >= 0);
    ALTER TABLE invoices ADD COLUMN currency TEXT;
    ALTER TABLE invoices ADD COLUMN sent_at INTEGER;
    ALTER TABLE invoices ADD COLUMN business_name TEXT;
    ALTER TABLE invoices ADD COLUMN business_address TEXT;
    ALTER TABLE invoices ADD COLUMN business_contact TEXT;
    CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    INSERT INTO settings (key, value) VALUES
        ('invoice_prefix', 'INV'),
        ('next_invoice_number', '1'),
        ('currency', 'USD'),
        ('business_name', ''),
        ('business_address', ''),
        ('business_contact', '');
    ",
    // v4: preserve the client header on invoice documents when client details change.
    "
    ALTER TABLE invoices ADD COLUMN client_name TEXT;
    ALTER TABLE invoices ADD COLUMN client_contact TEXT;
    ALTER TABLE invoices ADD COLUMN client_address TEXT;
    ",
];

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Applies any migrations newer than `PRAGMA user_version`.
    fn migrate(&mut self) -> Result<()> {
        let current: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (index + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    // ----- clients -----

    pub fn create_client(&self, name: &str, contact: &str, address: &str) -> Result<Id> {
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO clients (name, contact, address, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![name.trim(), contact, address, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Updates client details and terms; saved invoice headers and due dates remain unchanged.
    pub fn update_client(
        &self,
        id: Id,
        name: &str,
        contact: &str,
        address: &str,
        payment_terms_days: i64,
    ) -> Result<()> {
        if payment_terms_days < 0 {
            return Err(StorageError::Domain(DomainError::NegativeAmount));
        }
        let changed = self.conn.execute(
            "UPDATE clients
             SET name = ?1, contact = ?2, address = ?3, payment_terms_days = ?4, updated_at = ?5
             WHERE id = ?6",
            params![
                name.trim(),
                contact,
                address,
                payment_terms_days,
                now_unix(),
                id
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::NotFound { kind: "client", id });
        }
        Ok(())
    }

    pub fn list_clients(&self) -> Result<Vec<Client>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, contact, address, payment_terms_days FROM clients ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Client {
                id: row.get("id")?,
                name: row.get("name")?,
                contact: row.get("contact")?,
                address: row.get("address")?,
                payment_terms_days: row.get("payment_terms_days")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- projects -----

    pub fn create_project(
        &self,
        client_id: Id,
        name: &str,
        billing: BillingType,
        hourly_rate_cents: i64,
    ) -> Result<Id> {
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO projects
                (client_id, name, billing, hourly_rate_cents, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5)",
            params![
                client_id,
                name.trim(),
                billing_to_str(billing),
                hourly_rate_cents,
                now
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Updates a project's name and hourly rate without changing billing type or recorded totals.
    pub fn update_project_details(&self, id: Id, name: &str, hourly_rate_cents: i64) -> Result<()> {
        if hourly_rate_cents < 0 {
            return Err(StorageError::Domain(DomainError::NegativeAmount));
        }
        let changed = self.conn.execute(
            "UPDATE projects SET name = ?1, hourly_rate_cents = ?2, updated_at = ?3
             WHERE id = ?4",
            params![name.trim(), hourly_rate_cents, now_unix(), id],
        )?;
        if changed != 1 {
            return Err(StorageError::NotFound {
                kind: "project",
                id,
            });
        }
        Ok(())
    }

    pub fn get_project(&self, id: Id) -> Result<Project> {
        self.conn
            .query_row(
                "SELECT id, client_id, name, billing, hourly_rate_cents, status
                 FROM projects WHERE id = ?1",
                [id],
                row_to_project,
            )
            .optional()?
            .ok_or(StorageError::NotFound {
                kind: "project",
                id,
            })
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, client_id, name, billing, hourly_rate_cents, status
             FROM projects ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_project)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Total tracked seconds for stopped entries on a project.
    pub fn project_total_seconds(&self, project_id: Id) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(end_at - start_at), 0) FROM time_entries
             WHERE project_id = ?1 AND end_at IS NOT NULL",
            [project_id],
            |r| r.get(0),
        )?)
    }

    // ----- time entries -----

    pub fn running_timer(&self) -> Result<Option<TimeEntry>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, project_id, description, start_at, end_at, billable, invoice_id
                 FROM time_entries WHERE end_at IS NULL",
                [],
                row_to_entry,
            )
            .optional()?)
    }

    pub fn start_timer(&self, project_id: Id, description: &str, now: i64) -> Result<Id> {
        if self.running_timer()?.is_some() {
            return Err(StorageError::Conflict("a timer is already running"));
        }
        self.get_project(project_id)?;
        self.conn.execute(
            "INSERT INTO time_entries
                (project_id, description, start_at, end_at, billable, created_at, updated_at)
             VALUES (?1, ?2, ?3, NULL, 1, ?3, ?3)",
            params![project_id, description, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Stops the running timer. Returns the stopped entry.
    pub fn stop_timer(&self, now: i64) -> Result<TimeEntry> {
        let running = self
            .running_timer()?
            .ok_or(StorageError::Conflict("no timer is running"))?;
        domain::elapsed_seconds(running.start_at, now)?;
        let changed = self.conn.execute(
            "UPDATE time_entries SET end_at = ?1, updated_at = ?1
             WHERE id = ?2 AND end_at IS NULL",
            params![now, running.id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict("timer"));
        }
        self.get_entry(running.id)
    }

    pub fn add_manual_entry(
        &self,
        project_id: Id,
        description: &str,
        start_at: i64,
        end_at: i64,
        billable: bool,
    ) -> Result<Id> {
        domain::elapsed_seconds(start_at, end_at)?;
        self.get_project(project_id)?;
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO time_entries
                (project_id, description, start_at, end_at, billable, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                project_id,
                description,
                start_at,
                end_at,
                billable as i64,
                now
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Edits a stopped entry. Invoiced entries are rejected.
    pub fn update_entry(
        &self,
        id: Id,
        description: &str,
        start_at: i64,
        end_at: i64,
        billable: bool,
    ) -> Result<()> {
        let existing = self.get_entry(id)?;
        existing.ensure_editable()?;
        if existing.is_running() {
            return Err(StorageError::Conflict(
                "running timer must be stopped first",
            ));
        }
        domain::elapsed_seconds(start_at, end_at)?;
        let changed = self.conn.execute(
            "UPDATE time_entries
             SET description = ?1, start_at = ?2, end_at = ?3, billable = ?4, updated_at = ?5
             WHERE id = ?6 AND invoice_id IS NULL",
            params![
                description,
                start_at,
                end_at,
                billable as i64,
                now_unix(),
                id
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict("time entry"));
        }
        Ok(())
    }

    pub fn get_entry(&self, id: Id) -> Result<TimeEntry> {
        self.conn
            .query_row(
                "SELECT id, project_id, description, start_at, end_at, billable, invoice_id
                 FROM time_entries WHERE id = ?1",
                [id],
                row_to_entry,
            )
            .optional()?
            .ok_or(StorageError::NotFound {
                kind: "time entry",
                id,
            })
    }

    pub fn list_entries(&self, project_id: Id) -> Result<Vec<TimeEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, description, start_at, end_at, billable, invoice_id
             FROM time_entries WHERE project_id = ?1
             ORDER BY start_at DESC",
        )?;
        let rows = stmt.query_map([project_id], row_to_entry)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- milestones -----

    pub fn add_milestone(&self, project_id: Id, name: &str, amount_cents: i64) -> Result<Id> {
        self.get_project(project_id)?;
        let now = now_unix();
        self.conn.execute(
            "INSERT INTO milestones (project_id, name, amount_cents, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![project_id, name.trim(), amount_cents, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_milestones(&self, project_id: Id) -> Result<Vec<Milestone>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, name, amount_cents, invoice_id, delivered_at
             FROM milestones WHERE project_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([project_id], row_to_milestone)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_milestone(&self, id: Id) -> Result<Milestone> {
        self.conn
            .query_row(
                "SELECT id, project_id, name, amount_cents, invoice_id, delivered_at
                 FROM milestones WHERE id = ?1",
                [id],
                row_to_milestone,
            )
            .optional()?
            .ok_or(StorageError::NotFound {
                kind: "milestone",
                id,
            })
    }

    /// Changes a milestone's price. Rejected once the milestone is invoiced.
    pub fn update_milestone_amount(&self, id: Id, amount_cents: i64) -> Result<()> {
        let existing = self.get_milestone(id)?;
        existing.ensure_amount_editable()?;
        if amount_cents < 0 {
            return Err(StorageError::Domain(DomainError::NegativeAmount));
        }
        let changed = self.conn.execute(
            "UPDATE milestones SET amount_cents = ?1, updated_at = ?2
             WHERE id = ?3 AND invoice_id IS NULL",
            params![amount_cents, now_unix(), id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict("milestone"));
        }
        Ok(())
    }

    /// Marks a milestone delivered. Marking an already-delivered milestone is a no-op,
    /// so the original delivery date is kept.
    pub fn mark_milestone_delivered(&self, id: Id, now: i64) -> Result<()> {
        let existing = self.get_milestone(id)?;
        if existing.is_delivered() {
            return Ok(());
        }
        let changed = self.conn.execute(
            "UPDATE milestones SET delivered_at = ?1, updated_at = ?1
             WHERE id = ?2 AND delivered_at IS NULL",
            params![now, id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict("milestone"));
        }
        Ok(())
    }

    /// Sum of all milestone prices on a project. This is the fixed-price project total.
    pub fn project_milestone_total(&self, project_id: Id) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(amount_cents), 0) FROM milestones WHERE project_id = ?1",
            [project_id],
            |r| r.get(0),
        )?)
    }

    // ----- invoices -----

    /// Creates a draft invoice from the project's unbilled work and marks those
    /// sources as invoiced. All of it happens in one immediate transaction, so a
    /// source cannot be billed twice.
    pub fn create_invoice(&mut self, project_id: Id, issue_date: &str) -> Result<Id> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let project = query_project(&tx, project_id)?;
        // Terms and header are snapshotted now, so later client or settings
        // changes do not alter this invoice. The number is NOT assigned here; it
        // is assigned on send (see `send_invoice`).
        let (
            terms_days,
            currency,
            business_name,
            business_address,
            business_contact,
            client_name,
            client_contact,
            client_address,
        ): (i64, String, String, String, String, String, String, String) = tx.query_row(
            "SELECT c.payment_terms_days,
                    (SELECT value FROM settings WHERE key = 'currency'),
                    (SELECT value FROM settings WHERE key = 'business_name'),
                    (SELECT value FROM settings WHERE key = 'business_address'),
                    (SELECT value FROM settings WHERE key = 'business_contact'),
                    c.name, c.contact, c.address
             FROM clients c WHERE c.id = ?1",
            [project.client_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )?;
        let due_date = domain::add_days(issue_date, terms_days)?;
        let lines = match project.billing {
            BillingType::Hourly => {
                let entries = unbilled_entries(&tx, project_id)?;
                domain::hourly_invoice_lines(&project, &entries)?
            }
            BillingType::FixedPrice => {
                let milestones = unbilled_milestones(&tx, project_id)?;
                domain::milestone_invoice_lines(&project, &milestones)?
            }
        };
        let total = domain::invoice_total(&lines)?;
        let now = now_unix();

        tx.execute(
            "INSERT INTO invoices
                (client_id, project_id, number, status, issue_date, due_date,
                 total_cents, terms_days, currency, business_name,
                 business_address, business_contact, client_name, client_contact,
                 client_address, created_at, updated_at)
             VALUES (?1, ?2, NULL, 'draft', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     ?11, ?12, ?13, ?14, ?14)",
            params![
                project.client_id,
                project_id,
                issue_date,
                due_date,
                total,
                terms_days,
                currency,
                business_name,
                business_address,
                business_contact,
                client_name,
                client_contact,
                client_address,
                now
            ],
        )?;
        let invoice_id = tx.last_insert_rowid();

        for line in &lines {
            let (kind, source_id) = match line.source {
                LineSource::TimeEntry(id) => ("time_entry", id),
                LineSource::Milestone(id) => ("milestone", id),
            };
            tx.execute(
                "INSERT INTO invoice_lines
                    (invoice_id, description, quantity_seconds, amount_cents,
                     source_kind, source_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    invoice_id,
                    line.description,
                    line.quantity_seconds,
                    line.amount_cents,
                    kind,
                    source_id
                ],
            )?;
            // The `invoice_id IS NULL` guard means a source already billed by
            // another invoice cannot be claimed twice.
            let table = match line.source {
                LineSource::TimeEntry(_) => "time_entries",
                LineSource::Milestone(_) => "milestones",
            };
            let changed = tx.execute(
                &format!(
                    "UPDATE {table} SET invoice_id = ?1, updated_at = ?2
                          WHERE id = ?3 AND invoice_id IS NULL"
                ),
                params![invoice_id, now, source_id],
            )?;
            if changed != 1 {
                return Err(StorageError::Conflict("billable item"));
            }
        }

        tx.commit()?;
        Ok(invoice_id)
    }

    /// Sets the client's default payment terms in days. Applies to invoices
    /// created after the change; existing invoices keep their snapshot.
    pub fn set_client_payment_terms(&self, client_id: Id, days: i64) -> Result<()> {
        if days < 0 {
            return Err(StorageError::Domain(DomainError::NegativeAmount));
        }
        let changed = self.conn.execute(
            "UPDATE clients SET payment_terms_days = ?1, updated_at = ?2 WHERE id = ?3",
            params![days, now_unix(), client_id],
        )?;
        if changed != 1 {
            return Err(StorageError::NotFound {
                kind: "client",
                id: client_id,
            });
        }
        Ok(())
    }

    /// Writes a settings value (for example `currency` or `business_name`).
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn get_invoice(&self, id: Id) -> Result<Invoice> {
        self.conn
            .query_row(
                "SELECT id, client_id, project_id, number, status, issue_date, due_date,
                        total_cents, COALESCE(currency, 'USD') AS currency
                 FROM invoices WHERE id = ?1",
                [id],
                row_to_invoice,
            )
            .optional()?
            .ok_or(StorageError::NotFound {
                kind: "invoice",
                id,
            })
    }

    pub fn list_invoices(&self, project_id: Option<Id>) -> Result<Vec<Invoice>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, client_id, project_id, number, status, issue_date, due_date,
                    total_cents, COALESCE(currency, 'USD') AS currency
             FROM invoices
             WHERE (?1 IS NULL OR project_id = ?1)
             ORDER BY issue_date DESC, id DESC",
        )?;
        let rows = stmt.query_map([project_id], row_to_invoice)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn invoice_lines(&self, invoice_id: Id) -> Result<Vec<InvoiceLine>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_kind, source_id, description, quantity_seconds, amount_cents
             FROM invoice_lines WHERE invoice_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([invoice_id], |row| {
            let kind: String = row.get("source_kind")?;
            let id: Id = row.get("source_id")?;
            let source = match kind.as_str() {
                "time_entry" => LineSource::TimeEntry(id),
                "milestone" => LineSource::Milestone(id),
                other => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        Type::Text,
                        format!("unknown source kind: {other}").into(),
                    ));
                }
            };
            Ok(InvoiceLine {
                source,
                description: row.get("description")?,
                quantity_seconds: row.get("quantity_seconds")?,
                amount_cents: row.get("amount_cents")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn invoice_document(&self, id: Id) -> Result<InvoiceDocument> {
        let invoice = self.get_invoice(id)?;
        let client = self
            .conn
            .query_row(
                "SELECT c.id,
                        COALESCE(i.client_name, c.name) AS name,
                        COALESCE(i.client_contact, c.contact) AS contact,
                        COALESCE(i.client_address, c.address) AS address,
                        COALESCE(i.terms_days, c.payment_terms_days) AS payment_terms_days
                 FROM clients c JOIN invoices i ON i.client_id = c.id
                 WHERE i.id = ?1",
                [id],
                |row| {
                    Ok(Client {
                        id: row.get("id")?,
                        name: row.get("name")?,
                        contact: row.get("contact")?,
                        address: row.get("address")?,
                        payment_terms_days: row.get("payment_terms_days")?,
                    })
                },
            )
            .optional()?
            .ok_or(StorageError::NotFound {
                kind: "client",
                id: invoice.client_id,
            })?;
        let project = self.get_project(invoice.project_id)?;
        let (currency, business_name, business_address, business_contact): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = self.conn.query_row(
            "SELECT currency, business_name, business_address, business_contact
             FROM invoices WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        Ok(InvoiceDocument {
            invoice,
            client,
            project,
            currency: currency.unwrap_or_else(|| "USD".to_string()),
            business_name: business_name.unwrap_or_default(),
            business_address: business_address.unwrap_or_default(),
            business_contact: business_contact.unwrap_or_default(),
            lines: self.invoice_lines(id)?,
        })
    }

    /// Marks a draft as sent: assigns the next invoice number, sets `sent_at`,
    /// and moves the status to `sent`. This records status only; it does not
    /// deliver email or otherwise transmit the invoice.
    pub fn send_invoice(&mut self, id: Id, now: i64) -> Result<String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let invoice = tx
            .query_row(
                "SELECT id, client_id, project_id, number, status, issue_date, due_date,
                        total_cents, COALESCE(currency, 'USD') AS currency
                 FROM invoices WHERE id = ?1",
                [id],
                row_to_invoice,
            )
            .optional()?
            .ok_or(StorageError::NotFound {
                kind: "invoice",
                id,
            })?;
        domain::transition(invoice.status, InvoiceStatus::Sent)?;
        if invoice.number.is_some() {
            return Err(StorageError::Conflict("invoice"));
        }

        let prefix: String = tx.query_row(
            "SELECT value FROM settings WHERE key = 'invoice_prefix'",
            [],
            |r| r.get(0),
        )?;
        let next: i64 = tx.query_row(
            "SELECT CAST(value AS INTEGER) FROM settings WHERE key = 'next_invoice_number'",
            [],
            |r| r.get(0),
        )?;
        let number = format!("{prefix}-{next:04}");

        let changed = tx.execute(
            "UPDATE invoices SET number = ?1, status = 'sent', sent_at = ?2, updated_at = ?2
             WHERE id = ?3 AND status = 'draft' AND number IS NULL",
            params![number, now, id],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict("invoice"));
        }
        tx.execute(
            "UPDATE settings SET value = ?1 WHERE key = 'next_invoice_number'",
            [(next + 1).to_string()],
        )?;
        tx.commit()?;
        Ok(number)
    }

    /// Cancels a draft and releases its source entries/milestones back to the
    /// unbilled pool. Numbered or sent invoices cannot be cancelled this way.
    pub fn cancel_invoice_draft(&mut self, id: Id) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: Option<String> = tx
            .query_row("SELECT status FROM invoices WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(status) = status else {
            return Err(StorageError::NotFound {
                kind: "invoice",
                id,
            });
        };
        if status != "draft" {
            return Err(StorageError::Conflict(
                "only a draft invoice can be cancelled",
            ));
        }

        let now = now_unix();
        tx.execute(
            "UPDATE time_entries SET invoice_id = NULL, updated_at = ?1 WHERE invoice_id = ?2",
            params![now, id],
        )?;
        tx.execute(
            "UPDATE milestones SET invoice_id = NULL, updated_at = ?1 WHERE invoice_id = ?2",
            params![now, id],
        )?;
        tx.execute("DELETE FROM invoice_lines WHERE invoice_id = ?1", [id])?;
        let deleted = tx.execute(
            "DELETE FROM invoices WHERE id = ?1 AND status = 'draft'",
            [id],
        )?;
        if deleted != 1 {
            return Err(StorageError::Conflict("invoice"));
        }
        tx.commit()?;
        Ok(())
    }

    /// Creates a new unnumbered draft for the same client, copying the client's
    /// payment terms and the header snapshot. No lines are copied, and the
    /// number stays NULL because `number` is unique when set.
    pub fn duplicate_invoice_draft(&mut self, source_id: Id, issue_date: &str) -> Result<Id> {
        let source = self.get_invoice(source_id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (
            terms_days,
            currency,
            business_name,
            business_address,
            business_contact,
            client_name,
            client_contact,
            client_address,
        ): (
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
            String,
        ) = tx.query_row(
            "SELECT i.terms_days, i.currency, i.business_name, i.business_address,
                    i.business_contact, COALESCE(i.client_name, c.name),
                    COALESCE(i.client_contact, c.contact),
                    COALESCE(i.client_address, c.address)
             FROM invoices i JOIN clients c ON c.id = i.client_id WHERE i.id = ?1",
            [source.id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )?;
        let due_date = domain::add_days(issue_date, terms_days.unwrap_or(0))?;
        let now = now_unix();
        tx.execute(
            "INSERT INTO invoices
                (client_id, project_id, number, status, issue_date, due_date,
                 total_cents, terms_days, currency, business_name,
                 business_address, business_contact, client_name, client_contact,
                 client_address, created_at, updated_at)
             VALUES (?1, ?2, NULL, 'draft', ?3, ?4, 0, ?5, ?6, ?7, ?8, ?9,
                     ?10, ?11, ?12, ?13, ?13)",
            params![
                source.client_id,
                source.project_id,
                issue_date,
                due_date,
                terms_days,
                currency,
                business_name,
                business_address,
                business_contact,
                client_name,
                client_contact,
                client_address,
                now
            ],
        )?;
        let new_id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(new_id)
    }

    /// Moves an invoice forward (draft → sent → paid). Rejects other changes.
    pub fn set_invoice_status(&self, id: Id, to: InvoiceStatus) -> Result<()> {
        // Sending must go through `send_invoice`, which assigns the number.
        if to == InvoiceStatus::Sent {
            return Err(StorageError::Domain(DomainError::InvalidTransition {
                from: self.get_invoice(id)?.status,
                to,
            }));
        }
        let invoice = self.get_invoice(id)?;
        let next = domain::transition(invoice.status, to)?;
        let changed = self.conn.execute(
            "UPDATE invoices SET status = ?1, updated_at = ?2 WHERE id = ?3 AND status = ?4",
            params![
                status_to_str(next),
                now_unix(),
                id,
                status_to_str(invoice.status)
            ],
        )?;
        if changed != 1 {
            return Err(StorageError::Conflict("invoice"));
        }
        Ok(())
    }
}

// ----- helpers shared by the transaction and the connection -----

fn query_project(conn: &Connection, id: Id) -> Result<Project> {
    conn.query_row(
        "SELECT id, client_id, name, billing, hourly_rate_cents, status
         FROM projects WHERE id = ?1",
        [id],
        row_to_project,
    )
    .optional()?
    .ok_or(StorageError::NotFound {
        kind: "project",
        id,
    })
}

fn unbilled_entries(conn: &Connection, project_id: Id) -> Result<Vec<TimeEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, description, start_at, end_at, billable, invoice_id
         FROM time_entries
         WHERE project_id = ?1 AND invoice_id IS NULL AND end_at IS NOT NULL
         ORDER BY start_at",
    )?;
    let rows = stmt.query_map([project_id], row_to_entry)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn unbilled_milestones(conn: &Connection, project_id: Id) -> Result<Vec<Milestone>> {
    let mut stmt = conn.prepare(
        "SELECT id, project_id, name, amount_cents, invoice_id, delivered_at
         FROM milestones WHERE project_id = ?1 AND invoice_id IS NULL ORDER BY id",
    )?;
    let rows = stmt.query_map([project_id], row_to_milestone)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn row_to_project(row: &Row) -> rusqlite::Result<Project> {
    let billing_idx = row.as_ref().column_index("billing")?;
    let status_idx = row.as_ref().column_index("status")?;
    Ok(Project {
        id: row.get("id")?,
        client_id: row.get("client_id")?,
        name: row.get("name")?,
        billing: billing_from_str(&row.get::<_, String>("billing")?)
            .map_err(|s| conversion_error(billing_idx, s))?,
        hourly_rate_cents: row.get("hourly_rate_cents")?,
        status: match row.get::<_, String>("status")?.as_str() {
            "active" => ProjectStatus::Active,
            "completed" => ProjectStatus::Completed,
            other => return Err(conversion_error(status_idx, other.to_string())),
        },
    })
}

fn row_to_entry(row: &Row) -> rusqlite::Result<TimeEntry> {
    Ok(TimeEntry {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        description: row.get("description")?,
        start_at: row.get("start_at")?,
        end_at: row.get("end_at")?,
        billable: row.get::<_, i64>("billable")? != 0,
        invoice_id: row.get("invoice_id")?,
    })
}

fn row_to_milestone(row: &Row) -> rusqlite::Result<Milestone> {
    Ok(Milestone {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        amount_cents: row.get("amount_cents")?,
        invoice_id: row.get("invoice_id")?,
        delivered_at: row.get("delivered_at")?,
    })
}

fn row_to_invoice(row: &Row) -> rusqlite::Result<Invoice> {
    let status_idx = row.as_ref().column_index("status")?;
    let status_text: String = row.get("status")?;
    Ok(Invoice {
        id: row.get("id")?,
        client_id: row.get("client_id")?,
        project_id: row.get("project_id")?,
        number: row.get("number")?,
        status: status_from_str(&status_text).map_err(|s| conversion_error(status_idx, s))?,
        issue_date: row.get("issue_date")?,
        due_date: row.get("due_date")?,
        total_cents: row.get("total_cents")?,
        currency: row.get("currency")?,
    })
}

fn conversion_error(column: usize, detail: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, Type::Text, detail.into())
}

fn billing_to_str(b: BillingType) -> &'static str {
    match b {
        BillingType::Hourly => "hourly",
        BillingType::FixedPrice => "fixed_price",
    }
}

fn billing_from_str(s: &str) -> std::result::Result<BillingType, String> {
    match s {
        "hourly" => Ok(BillingType::Hourly),
        "fixed_price" => Ok(BillingType::FixedPrice),
        other => Err(format!("unknown billing type: {other}")),
    }
}

fn status_to_str(s: InvoiceStatus) -> &'static str {
    match s {
        InvoiceStatus::Draft => "draft",
        InvoiceStatus::Sent => "sent",
        InvoiceStatus::Paid => "paid",
    }
}

fn status_from_str(s: &str) -> std::result::Result<InvoiceStatus, String> {
    match s {
        "draft" => Ok(InvoiceStatus::Draft),
        "sent" => Ok(InvoiceStatus::Sent),
        "paid" => Ok(InvoiceStatus::Paid),
        other => Err(format!("unknown invoice status: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;
    const HOUR: i64 = 3_600;

    /// Store with one client and one hourly project at 100.00/hour.
    fn hourly_fixture() -> (Store, Id) {
        let store = Store::open_in_memory().unwrap();
        let client = store.create_client("Acme", "", "").unwrap();
        let project = store
            .create_project(client, "Website", BillingType::Hourly, 10_000)
            .unwrap();
        (store, project)
    }

    #[test]
    fn migrations_are_idempotent() {
        let mut store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
    }

    #[test]
    fn client_and_project_details_can_be_updated() {
        let (mut store, project_id) = hourly_fixture();
        let client_id = store.get_project(project_id).unwrap().client_id;
        store
            .add_manual_entry(project_id, "first", 0, HOUR, true)
            .unwrap();
        let old_invoice = store.create_invoice(project_id, "2026-11-01").unwrap();
        assert_eq!(
            store.invoice_document(old_invoice).unwrap().client.name,
            "Acme"
        );

        store
            .update_client(
                client_id,
                "Acme Studio",
                "hello@example.test",
                "1 Main St",
                14,
            )
            .unwrap();
        let client = store.list_clients().unwrap().remove(0);
        assert_eq!(client.name, "Acme Studio");
        assert_eq!(client.contact, "hello@example.test");
        assert_eq!(client.address, "1 Main St");
        assert_eq!(client.payment_terms_days, 14);
        let old_document = store.invoice_document(old_invoice).unwrap();
        assert_eq!(old_document.client.name, "Acme");
        assert_eq!(old_document.client.payment_terms_days, 30);

        store
            .update_project_details(project_id, "Website refresh", 12_500)
            .unwrap();
        let project = store.get_project(project_id).unwrap();
        assert_eq!(project.name, "Website refresh");
        assert_eq!(project.hourly_rate_cents, 12_500);
        assert_eq!(project.billing, BillingType::Hourly);

        store
            .add_manual_entry(project_id, "second", HOUR, 2 * HOUR, true)
            .unwrap();
        let new_invoice = store.create_invoice(project_id, "2026-11-02").unwrap();
        assert_eq!(
            store.invoice_document(new_invoice).unwrap().client.name,
            "Acme Studio"
        );
    }

    #[test]
    fn only_one_timer_can_run() {
        let (store, project) = hourly_fixture();
        store.start_timer(project, "first", 0).unwrap();
        assert!(matches!(
            store.start_timer(project, "second", 10),
            Err(StorageError::Conflict(_))
        ));
    }

    #[test]
    fn stop_timer_records_duration() {
        let (store, project) = hourly_fixture();
        store.start_timer(project, "work", 1_000).unwrap();
        let stopped = store.stop_timer(1_000 + HOUR).unwrap();
        assert_eq!(stopped.duration_seconds(), Some(HOUR));
        assert!(store.running_timer().unwrap().is_none());
    }

    #[test]
    fn stop_rejects_end_before_start() {
        let (store, project) = hourly_fixture();
        store.start_timer(project, "work", 1_000).unwrap();
        assert!(matches!(
            store.stop_timer(500),
            Err(StorageError::Domain(DomainError::EndBeforeStart))
        ));
    }

    #[test]
    fn project_totals_count_only_stopped_entries() {
        let (store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store
            .add_manual_entry(project, "b", 0, 1_800, true)
            .unwrap();
        store.start_timer(project, "running", 10 * DAY).unwrap();
        assert_eq!(store.project_total_seconds(project).unwrap(), HOUR + 1_800);
    }

    #[test]
    fn invoiced_entry_cannot_be_edited() {
        let (mut store, project) = hourly_fixture();
        let entry = store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store.create_invoice(project, "2026-11-01").unwrap();
        assert!(matches!(
            store.update_entry(entry, "changed", 0, 2 * HOUR, true),
            Err(StorageError::Domain(DomainError::EntryLocked(_)))
        ));
    }

    #[test]
    fn hourly_invoice_bills_only_unbilled_entries_once() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store
            .add_manual_entry(project, "b", HOUR, HOUR + 1_800, true)
            .unwrap();
        store
            .add_manual_entry(project, "skip", 0, HOUR, false)
            .unwrap();

        let first = store.create_invoice(project, "2026-11-01").unwrap();
        let invoice = store.get_invoice(first).unwrap();
        assert_eq!(invoice.total_cents, 10_000 + 5_000);
        assert_eq!(invoice.status, InvoiceStatus::Draft);
        assert_eq!(store.invoice_lines(first).unwrap().len(), 2);

        // Second attempt has nothing new to bill.
        assert!(matches!(
            store.create_invoice(project, "2026-11-02"),
            Err(StorageError::Domain(DomainError::NothingToInvoice))
        ));

        // Non-billable entry is still unbilled.
        let unbilled: Vec<_> = store
            .list_entries(project)
            .unwrap()
            .into_iter()
            .filter(|e| !e.is_invoiced())
            .collect();
        assert_eq!(unbilled.len(), 1);
        assert!(!unbilled[0].billable);
    }

    #[test]
    fn failed_invoice_leaves_nothing_behind() {
        // A running entry is never selected, so creating from a project with only
        // a running timer must fail and write nothing.
        let (mut store, project) = hourly_fixture();
        store.start_timer(project, "still running", 0).unwrap();
        assert!(store.create_invoice(project, "2026-11-01").is_err());
        assert!(store.list_invoices(None).unwrap().is_empty());
    }

    #[test]
    fn cancelling_a_draft_releases_its_billable_sources() {
        let (mut store, project) = hourly_fixture();
        let entry = store
            .add_manual_entry(project, "work", 0, HOUR, true)
            .unwrap();
        let invoice = store.create_invoice(project, "2026-11-01").unwrap();
        assert_eq!(store.get_entry(entry).unwrap().invoice_id, Some(invoice));

        store.cancel_invoice_draft(invoice).unwrap();
        assert_eq!(store.get_entry(entry).unwrap().invoice_id, None);
        assert!(matches!(
            store.get_invoice(invoice),
            Err(StorageError::NotFound {
                kind: "invoice",
                ..
            })
        ));

        let replacement = store.create_invoice(project, "2026-11-02").unwrap();
        assert_eq!(store.get_invoice(replacement).unwrap().total_cents, 10_000);
    }

    #[test]
    fn fixed_price_invoice_uses_milestones() {
        let store = Store::open_in_memory().unwrap();
        let client = store.create_client("Acme", "", "").unwrap();
        let project = store
            .create_project(client, "Redesign", BillingType::FixedPrice, 0)
            .unwrap();
        let design = store.add_milestone(project, "Design", 40_000).unwrap();
        let build = store.add_milestone(project, "Build", 60_000).unwrap();
        store.mark_milestone_delivered(design, 1).unwrap();
        store.mark_milestone_delivered(build, 1).unwrap();

        let mut store = store;
        let id = store.create_invoice(project, "2026-11-01").unwrap();
        assert_eq!(store.get_invoice(id).unwrap().total_cents, 100_000);

        let lines = store.invoice_lines(id).unwrap();
        assert_eq!(lines[0].source, LineSource::Milestone(design));
        assert_eq!(lines.len(), 2);

        // Milestones are now billed.
        assert!(matches!(
            store.create_invoice(project, "2026-11-02"),
            Err(StorageError::Domain(DomainError::NothingToInvoice))
        ));
    }

    fn fixed_fixture() -> (Store, Id) {
        let store = Store::open_in_memory().unwrap();
        let client = store.create_client("Acme", "", "").unwrap();
        let project = store
            .create_project(client, "Redesign", BillingType::FixedPrice, 0)
            .unwrap();
        (store, project)
    }

    #[test]
    fn undelivered_milestone_cannot_be_invoiced() {
        let (mut store, project) = fixed_fixture();
        store.add_milestone(project, "Design", 40_000).unwrap();
        assert!(matches!(
            store.create_invoice(project, "2026-11-01"),
            Err(StorageError::Domain(DomainError::NothingToInvoice))
        ));
        assert!(store.list_invoices(None).unwrap().is_empty());
    }

    #[test]
    fn delivered_milestone_price_can_change_until_invoiced() {
        let (mut store, project) = fixed_fixture();
        let m = store.add_milestone(project, "Design", 40_000).unwrap();
        store.mark_milestone_delivered(m, 1).unwrap();
        store.update_milestone_amount(m, 45_000).unwrap();
        assert_eq!(store.get_milestone(m).unwrap().amount_cents, 45_000);

        store.create_invoice(project, "2026-11-01").unwrap();
        assert!(matches!(
            store.update_milestone_amount(m, 50_000),
            Err(StorageError::Domain(DomainError::MilestoneLocked(_)))
        ));
        assert_eq!(store.get_milestone(m).unwrap().amount_cents, 45_000);
    }

    #[test]
    fn project_total_sums_milestones() {
        let (store, project) = fixed_fixture();
        store.add_milestone(project, "Design", 40_000).unwrap();
        store.add_milestone(project, "Build", 60_000).unwrap();
        assert_eq!(store.project_milestone_total(project).unwrap(), 100_000);
    }

    #[test]
    fn marking_delivered_keeps_original_date() {
        let (store, project) = fixed_fixture();
        let m = store.add_milestone(project, "Design", 40_000).unwrap();
        store.mark_milestone_delivered(m, 100).unwrap();
        store.mark_milestone_delivered(m, 999).unwrap();
        assert_eq!(store.get_milestone(m).unwrap().delivered_at, Some(100));
    }

    #[test]
    fn new_draft_without_number_is_stored_as_null() {
        // Duplicate must not copy the number, because `number` is unique when set.
        // This checks only that a draft created without a number stores NULL next to an
        // existing numbered invoice. Duplicate has its own tests; this one covers only
        // the NULL number on a plain new draft.
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let first = store.create_invoice(project, "2026-11-01").unwrap();
        store.send_invoice(first, 1).unwrap();
        assert!(store.get_invoice(first).unwrap().number.is_some());
        store
            .add_manual_entry(project, "b", HOUR, 2 * HOUR, true)
            .unwrap();
        let second = store.create_invoice(project, "2026-11-02").unwrap();
        assert_eq!(store.get_invoice(second).unwrap().number, None);
    }

    #[test]
    fn invoice_status_moves_forward_only() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let id = store.create_invoice(project, "2026-11-01").unwrap();

        assert!(store.set_invoice_status(id, InvoiceStatus::Paid).is_err());
        store.send_invoice(id, 1).unwrap();
        store.set_invoice_status(id, InvoiceStatus::Paid).unwrap();
        assert_eq!(store.get_invoice(id).unwrap().status, InvoiceStatus::Paid);
        assert!(store.set_invoice_status(id, InvoiceStatus::Sent).is_err());
    }

    #[test]
    fn due_date_is_derived_from_client_terms() {
        let (mut store, project) = hourly_fixture();
        let client = store.get_project(project).unwrap().client_id;
        store.set_client_payment_terms(client, 14).unwrap();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let id = store.create_invoice(project, "2026-11-20").unwrap();
        let invoice = store.get_invoice(id).unwrap();
        assert_eq!(invoice.due_date, "2026-12-04");
    }

    #[test]
    fn bad_issue_date_is_rejected_before_writing() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        assert!(store.create_invoice(project, "1 Nov 2026").is_err());
        assert!(store.list_invoices(None).unwrap().is_empty());
    }

    #[test]
    fn sent_invoices_get_sequential_numbers() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store
            .add_manual_entry(project, "b", HOUR, 2 * HOUR, true)
            .unwrap();
        let first = store.create_invoice(project, "2026-11-01").unwrap();
        store
            .add_manual_entry(project, "c", 2 * HOUR, 3 * HOUR, true)
            .unwrap();
        let second = store.create_invoice(project, "2026-11-02").unwrap();
        assert_eq!(store.send_invoice(first, 1).unwrap(), "INV-0001");
        assert_eq!(store.send_invoice(second, 2).unwrap(), "INV-0002");
    }

    #[test]
    fn duplicate_copies_terms_and_leaves_number_and_lines_empty() {
        let (mut store, project) = hourly_fixture();
        let client = store.get_project(project).unwrap().client_id;
        store.set_client_payment_terms(client, 7).unwrap();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let source = store.create_invoice(project, "2026-11-01").unwrap();
        store.send_invoice(source, 1).unwrap();

        let copy = store.duplicate_invoice_draft(source, "2026-12-01").unwrap();
        let invoice = store.get_invoice(copy).unwrap();
        assert_eq!(invoice.status, InvoiceStatus::Draft);
        assert_eq!(invoice.number, None);
        assert_eq!(invoice.due_date, "2026-12-08");
        assert_eq!(invoice.total_cents, 0);
        assert!(store.invoice_lines(copy).unwrap().is_empty());
        // Source is untouched.
        assert!(store.get_invoice(source).unwrap().number.is_some());
    }

    #[test]
    fn sending_twice_is_rejected() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let id = store.create_invoice(project, "2026-11-01").unwrap();
        store.send_invoice(id, 1).unwrap();
        assert!(store.send_invoice(id, 2).is_err());
        // The failed second send must not consume a number.
        store
            .add_manual_entry(project, "b", HOUR, 2 * HOUR, true)
            .unwrap();
        let next = store.create_invoice(project, "2026-11-02").unwrap();
        assert_eq!(store.send_invoice(next, 3).unwrap(), "INV-0002");
    }

    #[test]
    fn failed_send_rolls_back_number_and_lines() {
        // A second send of the same invoice must fail without consuming a number.
        // A failed create after a successful send must leave nothing unbilled.
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let id = store.create_invoice(project, "2026-11-01").unwrap();
        store.send_invoice(id, 1).unwrap();
        store
            .add_manual_entry(project, "b", HOUR, 2 * HOUR, true)
            .unwrap();
        assert!(store.send_invoice(id, 2).is_err());
        let unbilled: Vec<_> = store
            .list_entries(project)
            .unwrap()
            .into_iter()
            .filter(|e| !e.is_invoiced())
            .collect();
        assert_eq!(unbilled.len(), 1, "second entry must still be unbilled");
        assert_eq!(store.list_invoices(None).unwrap().len(), 1);
        let next = store.create_invoice(project, "2026-11-02").unwrap();
        assert_eq!(store.send_invoice(next, 3).unwrap(), "INV-0002");
    }

    #[test]
    fn duplicate_copies_header_snapshot_not_current_settings() {
        let (mut store, project) = hourly_fixture();
        store.set_setting("business_name", "Old Name").unwrap();
        store.set_setting("currency", "EUR").unwrap();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let source = store.create_invoice(project, "2026-11-01").unwrap();
        // Settings and client details change after the source was created.
        store.set_setting("business_name", "New Name").unwrap();
        store.set_setting("currency", "USD").unwrap();
        let client_id = store.get_project(project).unwrap().client_id;
        store
            .update_client(
                client_id,
                "Updated Acme",
                "new@example.test",
                "2 Main St",
                14,
            )
            .unwrap();

        let copy = store.duplicate_invoice_draft(source, "2026-12-01").unwrap();
        let header = |id: Id| -> (String, String) {
            store
                .conn
                .query_row(
                    "SELECT business_name, currency FROM invoices WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0).unwrap(), r.get(1).unwrap())),
                )
                .unwrap()
        };
        assert_eq!(header(source), ("Old Name".to_string(), "EUR".to_string()));
        assert_eq!(header(copy), ("Old Name".to_string(), "EUR".to_string()));
        assert_eq!(store.get_invoice(source).unwrap().currency, "EUR");
        assert_eq!(store.invoice_document(source).unwrap().client.name, "Acme");
        assert_eq!(store.invoice_document(copy).unwrap().client.name, "Acme");
    }

    #[test]
    fn invoice_currency_round_trips_and_legacy_null_defaults_to_usd() {
        let (mut store, project) = hourly_fixture();
        store.set_setting("currency", "EUR").unwrap();
        store
            .add_manual_entry(project, "work", 0, HOUR, true)
            .unwrap();
        let id = store.create_invoice(project, "2026-11-01").unwrap();

        assert_eq!(store.get_invoice(id).unwrap().currency, "EUR");
        assert_eq!(store.list_invoices(None).unwrap()[0].currency, "EUR");

        store
            .conn
            .execute(
                "UPDATE invoices
                 SET currency = NULL, client_name = NULL, client_contact = NULL, client_address = NULL
                 WHERE id = ?1",
                [id],
            )
            .unwrap();
        assert_eq!(store.get_invoice(id).unwrap().currency, "USD");
        assert_eq!(store.list_invoices(None).unwrap()[0].currency, "USD");
        let document = store.invoice_document(id).unwrap();
        assert_eq!(document.currency, "USD");
        assert_eq!(document.client.name, "Acme");
    }
}
