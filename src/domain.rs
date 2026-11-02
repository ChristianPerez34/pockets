//! Core rules for Pockets. Pure logic with no I/O, so it can be tested directly.
//!
//! Money is stored as integer cents. Time is stored as Unix seconds.

use std::fmt;

pub type Id = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingType {
    /// Billed per tracked hour at the project's hourly rate.
    Hourly,
    /// Billed per milestone. Tracked time does not change the invoice amount.
    FixedPrice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStatus {
    Active,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvoiceStatus {
    Draft,
    Sent,
    Paid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// An end time is before its start time.
    EndBeforeStart,
    /// A rate or amount is negative.
    NegativeAmount,
    /// Integer arithmetic overflowed.
    Overflow,
    /// The invoice status change is not allowed.
    InvalidTransition {
        from: InvoiceStatus,
        to: InvoiceStatus,
    },
    /// Hourly invoicing was requested for a fixed-price project, or the reverse.
    WrongBillingType {
        expected: BillingType,
        actual: BillingType,
    },
    /// There are no unbilled items to put on an invoice.
    NothingToInvoice,
    /// The entry is already on an invoice and cannot be edited.
    EntryLocked(Id),
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndBeforeStart => write!(f, "end time is before start time"),
            Self::NegativeAmount => write!(f, "amount must not be negative"),
            Self::Overflow => write!(f, "amount is too large"),
            Self::InvalidTransition { from, to } => {
                write!(f, "cannot change invoice status from {from:?} to {to:?}")
            }
            Self::WrongBillingType { expected, actual } => write!(
                f,
                "project bills as {actual:?}, but this action needs {expected:?}"
            ),
            Self::NothingToInvoice => write!(f, "there is nothing unbilled to invoice"),
            Self::EntryLocked(id) => write!(f, "time entry {id} is already invoiced"),
        }
    }
}

impl std::error::Error for DomainError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: Id,
    pub client_id: Id,
    pub name: String,
    pub billing: BillingType,
    pub hourly_rate_cents: i64,
    pub status: ProjectStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeEntry {
    pub id: Id,
    pub project_id: Id,
    pub description: String,
    pub start_at: i64,
    /// `None` while the timer is running.
    pub end_at: Option<i64>,
    pub billable: bool,
    /// Set once the entry is included in an invoice. Used as the "invoiced" marker.
    pub invoice_id: Option<Id>,
}

impl TimeEntry {
    pub fn is_running(&self) -> bool {
        self.end_at.is_none()
    }

    pub fn is_invoiced(&self) -> bool {
        self.invoice_id.is_some()
    }

    /// Duration in seconds, or `None` if the timer is still running.
    pub fn duration_seconds(&self) -> Option<i64> {
        self.end_at.map(|end| end - self.start_at)
    }

    /// Returns an error if the entry may not be edited because it is already invoiced.
    pub fn ensure_editable(&self) -> Result<(), DomainError> {
        if self.is_invoiced() {
            Err(DomainError::EntryLocked(self.id))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Milestone {
    pub id: Id,
    pub project_id: Id,
    pub name: String,
    pub amount_cents: i64,
    pub invoice_id: Option<Id>,
}

impl Milestone {
    pub fn is_invoiced(&self) -> bool {
        self.invoice_id.is_some()
    }
}

/// Where an invoice line came from. Used to prevent billing the same source twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSource {
    TimeEntry(Id),
    Milestone(Id),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvoiceLine {
    pub source: LineSource,
    pub description: String,
    /// Present for hourly lines.
    pub quantity_seconds: Option<i64>,
    pub amount_cents: i64,
}

/// Amount for `seconds` at `rate_cents_per_hour`, rounded half-up to the cent.
pub fn amount_for_seconds(seconds: i64, rate_cents_per_hour: i64) -> Result<i64, DomainError> {
    if seconds < 0 || rate_cents_per_hour < 0 {
        return Err(DomainError::NegativeAmount);
    }
    let product = seconds
        .checked_mul(rate_cents_per_hour)
        .ok_or(DomainError::Overflow)?;
    // Add half of the divisor (3600 / 2) before dividing to round half-up.
    product
        .checked_add(1800)
        .map(|with_half| with_half / 3600)
        .ok_or(DomainError::Overflow)
}

/// Seconds between two Unix timestamps. Rejects an end before the start.
pub fn elapsed_seconds(start_at: i64, end_at: i64) -> Result<i64, DomainError> {
    if end_at < start_at {
        Err(DomainError::EndBeforeStart)
    } else {
        Ok(end_at - start_at)
    }
}

/// Lines for an hourly project: one per stopped, billable, uninvoiced entry.
/// Running entries are skipped because their length is not known yet.
pub fn hourly_invoice_lines(
    project: &Project,
    entries: &[TimeEntry],
) -> Result<Vec<InvoiceLine>, DomainError> {
    require_billing(project, BillingType::Hourly)?;

    let mut lines = Vec::new();
    for entry in entries {
        if entry.project_id != project.id
            || !entry.billable
            || entry.is_invoiced()
            || entry.is_running()
        {
            continue;
        }
        let seconds = entry.duration_seconds().unwrap_or(0);
        lines.push(InvoiceLine {
            source: LineSource::TimeEntry(entry.id),
            description: entry.description.clone(),
            quantity_seconds: Some(seconds),
            amount_cents: amount_for_seconds(seconds, project.hourly_rate_cents)?,
        });
    }

    if lines.is_empty() {
        Err(DomainError::NothingToInvoice)
    } else {
        Ok(lines)
    }
}

/// Lines for a fixed-price project: one per uninvoiced milestone.
pub fn milestone_invoice_lines(
    project: &Project,
    milestones: &[Milestone],
) -> Result<Vec<InvoiceLine>, DomainError> {
    require_billing(project, BillingType::FixedPrice)?;

    let lines: Vec<InvoiceLine> = milestones
        .iter()
        .filter(|m| m.project_id == project.id && !m.is_invoiced())
        .map(|m| InvoiceLine {
            source: LineSource::Milestone(m.id),
            description: m.name.clone(),
            quantity_seconds: None,
            amount_cents: m.amount_cents,
        })
        .collect();

    if lines.is_empty() {
        Err(DomainError::NothingToInvoice)
    } else {
        Ok(lines)
    }
}

/// Sum of line amounts, with overflow checking.
pub fn invoice_total(lines: &[InvoiceLine]) -> Result<i64, DomainError> {
    lines.iter().try_fold(0i64, |acc, line| {
        acc.checked_add(line.amount_cents)
            .ok_or(DomainError::Overflow)
    })
}

/// Allowed status changes: draft → sent → paid. Nothing moves backwards.
pub fn transition(from: InvoiceStatus, to: InvoiceStatus) -> Result<InvoiceStatus, DomainError> {
    use InvoiceStatus::*;
    match (from, to) {
        (Draft, Sent) | (Sent, Paid) => Ok(to),
        _ => Err(DomainError::InvalidTransition { from, to }),
    }
}

fn require_billing(project: &Project, expected: BillingType) -> Result<(), DomainError> {
    if project.billing == expected {
        Ok(())
    } else {
        Err(DomainError::WrongBillingType {
            expected,
            actual: project.billing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(billing: BillingType, rate: i64) -> Project {
        Project {
            id: 1,
            client_id: 1,
            name: "Site".into(),
            billing,
            hourly_rate_cents: rate,
            status: ProjectStatus::Active,
        }
    }

    fn entry(id: Id, start: i64, end: Option<i64>) -> TimeEntry {
        TimeEntry {
            id,
            project_id: 1,
            description: format!("entry {id}"),
            start_at: start,
            end_at: end,
            billable: true,
            invoice_id: None,
        }
    }

    #[test]
    fn one_hour_is_exactly_the_rate() {
        assert_eq!(amount_for_seconds(3600, 10_000), Ok(10_000));
    }

    #[test]
    fn half_cent_rounds_up() {
        // 30 minutes at 100.01 per hour is 50.005, which rounds to 50.01.
        assert_eq!(amount_for_seconds(1800, 10_001), Ok(5_001));
    }

    #[test]
    fn partial_hours_are_exact() {
        // 90 seconds at 90.00 per hour is 2.25.
        assert_eq!(amount_for_seconds(90, 9_000), Ok(225));
    }

    #[test]
    fn zero_is_zero() {
        assert_eq!(amount_for_seconds(0, 9_000), Ok(0));
    }

    #[test]
    fn negative_inputs_are_rejected() {
        assert_eq!(amount_for_seconds(-1, 100), Err(DomainError::NegativeAmount));
        assert_eq!(amount_for_seconds(1, -100), Err(DomainError::NegativeAmount));
    }

    #[test]
    fn overflow_is_reported_not_wrapped() {
        assert_eq!(amount_for_seconds(i64::MAX, 2), Err(DomainError::Overflow));
    }

    #[test]
    fn elapsed_rejects_end_before_start() {
        assert_eq!(elapsed_seconds(100, 50), Err(DomainError::EndBeforeStart));
        assert_eq!(elapsed_seconds(100, 160), Ok(60));
    }

    #[test]
    fn hourly_lines_skip_running_invoiced_and_non_billable() {
        let p = project(BillingType::Hourly, 5_000);
        let mut invoiced = entry(2, 0, Some(3600));
        invoiced.invoice_id = Some(9);
        let mut non_billable = entry(3, 0, Some(3600));
        non_billable.billable = false;
        let entries = vec![
            entry(1, 0, Some(3600)),
            invoiced,
            non_billable,
            entry(4, 0, None),
        ];

        let lines = hourly_invoice_lines(&p, &entries).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].source, LineSource::TimeEntry(1));
        assert_eq!(lines[0].amount_cents, 5_000);
        assert_eq!(lines[0].quantity_seconds, Some(3600));
    }

    #[test]
    fn hourly_lines_ignore_other_projects() {
        let p = project(BillingType::Hourly, 5_000);
        let mut other = entry(1, 0, Some(3600));
        other.project_id = 99;
        assert_eq!(
            hourly_invoice_lines(&p, &[other]),
            Err(DomainError::NothingToInvoice)
        );
    }

    #[test]
    fn empty_hourly_selection_is_an_error() {
        let p = project(BillingType::Hourly, 5_000);
        assert_eq!(hourly_invoice_lines(&p, &[]), Err(DomainError::NothingToInvoice));
    }

    #[test]
    fn hourly_invoicing_refuses_fixed_price_projects() {
        let p = project(BillingType::FixedPrice, 5_000);
        assert_eq!(
            hourly_invoice_lines(&p, &[entry(1, 0, Some(3600))]),
            Err(DomainError::WrongBillingType {
                expected: BillingType::Hourly,
                actual: BillingType::FixedPrice,
            })
        );
    }

    #[test]
    fn milestone_lines_skip_invoiced() {
        let p = project(BillingType::FixedPrice, 0);
        let milestones = vec![
            Milestone {
                id: 1,
                project_id: 1,
                name: "Design".into(),
                amount_cents: 40_000,
                invoice_id: None,
            },
            Milestone {
                id: 2,
                project_id: 1,
                name: "Build".into(),
                amount_cents: 60_000,
                invoice_id: Some(7),
            },
        ];
        let lines = milestone_invoice_lines(&p, &milestones).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].source, LineSource::Milestone(1));
        assert_eq!(lines[0].quantity_seconds, None);
    }

    #[test]
    fn milestone_invoicing_refuses_hourly_projects() {
        let p = project(BillingType::Hourly, 5_000);
        assert!(matches!(
            milestone_invoice_lines(&p, &[]),
            Err(DomainError::WrongBillingType { .. })
        ));
    }

    #[test]
    fn total_sums_lines() {
        let lines = vec![
            InvoiceLine {
                source: LineSource::TimeEntry(1),
                description: String::new(),
                quantity_seconds: Some(3600),
                amount_cents: 5_000,
            },
            InvoiceLine {
                source: LineSource::Milestone(2),
                description: String::new(),
                quantity_seconds: None,
                amount_cents: 12_345,
            },
        ];
        assert_eq!(invoice_total(&lines), Ok(17_345));
        assert_eq!(invoice_total(&[]), Ok(0));
    }

    #[test]
    fn status_moves_forward_only() {
        use InvoiceStatus::*;
        assert_eq!(transition(Draft, Sent), Ok(Sent));
        assert_eq!(transition(Sent, Paid), Ok(Paid));
        assert!(transition(Draft, Paid).is_err());
        assert!(transition(Sent, Draft).is_err());
        assert!(transition(Paid, Sent).is_err());
        assert!(transition(Draft, Draft).is_err());
    }

    #[test]
    fn invoiced_entries_are_locked() {
        let mut e = entry(5, 0, Some(60));
        assert!(e.ensure_editable().is_ok());
        e.invoice_id = Some(1);
        assert_eq!(e.ensure_editable(), Err(DomainError::EntryLocked(5)));
    }
}
