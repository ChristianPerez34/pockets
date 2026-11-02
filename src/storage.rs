//! SQLite persistence for Pockets.
//!
//! Business rules live in `domain`. This module loads records, calls the domain
//! functions, and writes results. Multi-row changes run in a single transaction.

use std::fmt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, Row, TransactionBehavior};

use crate::domain::{
    self, BillingType, DomainError, Id, InvoiceLine, InvoiceStatus, LineSource, Milestone,
    Project, ProjectStatus, TimeEntry,
};

#[derive(Debug)]
pub enum StorageError {
    Sql(rusqlite::Error),
    Domain(DomainError),
    NotFound { kind: &'static str, id: Id },
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

    pub fn list_clients(&self) -> Result<Vec<Client>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, contact, address FROM clients ORDER BY name")?;
        let rows = stmt.query_map([], |row| {
            Ok(Client {
                id: row.get("id")?,
                name: row.get("name")?,
                contact: row.get("contact")?,
                address: row.get("address")?,
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
            params![project_id, description, start_at, end_at, billable as i64, now],
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
            return Err(StorageError::Conflict("running timer must be stopped first"));
        }
        domain::elapsed_seconds(start_at, end_at)?;
        let changed = self.conn.execute(
            "UPDATE time_entries
             SET description = ?1, start_at = ?2, end_at = ?3, billable = ?4, updated_at = ?5
             WHERE id = ?6 AND invoice_id IS NULL",
            params![description, start_at, end_at, billable as i64, now_unix(), id],
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
            "SELECT id, project_id, name, amount_cents, invoice_id
             FROM milestones WHERE project_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([project_id], row_to_milestone)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----- invoices -----

    /// Creates a draft invoice from the project's unbilled work and marks those
    /// sources as invoiced. All of it happens in one immediate transaction, so a
    /// source cannot be billed twice.
    pub fn create_invoice(
        &mut self,
        project_id: Id,
        issue_date: &str,
        due_date: &str,
        number: Option<&str>,
    ) -> Result<Id> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let project = query_project(&tx, project_id)?;
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
                 total_cents, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7, ?7)",
            params![
                project.client_id,
                project_id,
                number,
                issue_date,
                due_date,
                total,
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
                &format!("UPDATE {table} SET invoice_id = ?1, updated_at = ?2
                          WHERE id = ?3 AND invoice_id IS NULL"),
                params![invoice_id, now, source_id],
            )?;
            if changed != 1 {
                return Err(StorageError::Conflict("billable item"));
            }
        }

        tx.commit()?;
        Ok(invoice_id)
    }

    pub fn get_invoice(&self, id: Id) -> Result<Invoice> {
        self.conn
            .query_row(
                "SELECT id, client_id, project_id, number, status, issue_date, due_date, total_cents
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
            "SELECT id, client_id, project_id, number, status, issue_date, due_date, total_cents
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
                    ))
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

    /// Moves an invoice forward (draft → sent → paid). Rejects other changes.
    pub fn set_invoice_status(&self, id: Id, to: InvoiceStatus) -> Result<()> {
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
        "SELECT id, project_id, name, amount_cents, invoice_id
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
        store.add_manual_entry(project, "b", 0, 1_800, true).unwrap();
        store.start_timer(project, "running", 10 * DAY).unwrap();
        assert_eq!(store.project_total_seconds(project).unwrap(), HOUR + 1_800);
    }

    #[test]
    fn invoiced_entry_cannot_be_edited() {
        let (mut store, project) = hourly_fixture();
        let entry = store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store
            .create_invoice(project, "2026-11-01", "2026-11-30", None)
            .unwrap();
        assert!(matches!(
            store.update_entry(entry, "changed", 0, 2 * HOUR, true),
            Err(StorageError::Domain(DomainError::EntryLocked(_)))
        ));
    }

    #[test]
    fn hourly_invoice_bills_only_unbilled_entries_once() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store.add_manual_entry(project, "b", HOUR, HOUR + 1_800, true).unwrap();
        store.add_manual_entry(project, "skip", 0, HOUR, false).unwrap();

        let first = store
            .create_invoice(project, "2026-11-01", "2026-11-30", Some("INV-1"))
            .unwrap();
        let invoice = store.get_invoice(first).unwrap();
        assert_eq!(invoice.total_cents, 10_000 + 5_000);
        assert_eq!(invoice.status, InvoiceStatus::Draft);
        assert_eq!(store.invoice_lines(first).unwrap().len(), 2);

        // Second attempt has nothing new to bill.
        assert!(matches!(
            store.create_invoice(project, "2026-11-02", "2026-11-30", None),
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
        assert!(store
            .create_invoice(project, "2026-11-01", "2026-11-30", None)
            .is_err());
        assert!(store.list_invoices(None).unwrap().is_empty());
    }

    #[test]
    fn fixed_price_invoice_uses_milestones() {
        let store = Store::open_in_memory().unwrap();
        let client = store.create_client("Acme", "", "").unwrap();
        let project = store
            .create_project(client, "Redesign", BillingType::FixedPrice, 0)
            .unwrap();
        let design = store.add_milestone(project, "Design", 40_000).unwrap();
        store.add_milestone(project, "Build", 60_000).unwrap();

        let mut store = store;
        let id = store
            .create_invoice(project, "2026-11-01", "2026-11-30", None)
            .unwrap();
        assert_eq!(store.get_invoice(id).unwrap().total_cents, 100_000);

        let lines = store.invoice_lines(id).unwrap();
        assert_eq!(lines[0].source, LineSource::Milestone(design));
        assert_eq!(lines.len(), 2);

        // Milestones are now billed.
        assert!(matches!(
            store.create_invoice(project, "2026-11-02", "2026-11-30", None),
            Err(StorageError::Domain(DomainError::NothingToInvoice))
        ));
    }

    #[test]
    fn invoice_status_moves_forward_only() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        let id = store
            .create_invoice(project, "2026-11-01", "2026-11-30", None)
            .unwrap();

        assert!(store.set_invoice_status(id, InvoiceStatus::Paid).is_err());
        store.set_invoice_status(id, InvoiceStatus::Sent).unwrap();
        store.set_invoice_status(id, InvoiceStatus::Paid).unwrap();
        assert_eq!(store.get_invoice(id).unwrap().status, InvoiceStatus::Paid);
        assert!(store.set_invoice_status(id, InvoiceStatus::Sent).is_err());
    }

    #[test]
    fn schema_rejects_due_before_issue() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        assert!(store
            .create_invoice(project, "2026-11-10", "2026-11-01", None)
            .is_err());
    }

    #[test]
    fn schema_rejects_bad_date_format() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        assert!(store.create_invoice(project, "1 Nov 2026", "2026-11-30", None).is_err());
    }

    #[test]
    fn duplicate_invoice_numbers_are_rejected() {
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store.add_manual_entry(project, "b", HOUR, 2 * HOUR, true).unwrap();
        store
            .create_invoice(project, "2026-11-01", "2026-11-30", Some("INV-1"))
            .unwrap();
        assert!(store
            .create_invoice(project, "2026-11-01", "2026-11-30", Some("INV-1"))
            .is_err());
    }

    #[test]
    fn failed_invoice_rolls_back_line_inserts() {
        // The duplicate number makes the INSERT fail after lines would be written
        // for a second invoice. The whole transaction must roll back.
        let (mut store, project) = hourly_fixture();
        store.add_manual_entry(project, "a", 0, HOUR, true).unwrap();
        store
            .create_invoice(project, "2026-11-01", "2026-11-30", Some("INV-1"))
            .unwrap();
        store.add_manual_entry(project, "b", HOUR, 2 * HOUR, true).unwrap();
        assert!(store
            .create_invoice(project, "2026-11-01", "2026-11-30", Some("INV-1"))
            .is_err());
        let unbilled: Vec<_> = store
            .list_entries(project)
            .unwrap()
            .into_iter()
            .filter(|e| !e.is_invoiced())
            .collect();
        assert_eq!(unbilled.len(), 1, "second entry must still be unbilled");
        assert_eq!(store.list_invoices(None).unwrap().len(), 1);
    }
}
