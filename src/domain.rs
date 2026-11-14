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
    /// The milestone is already on an invoice, so its price cannot change.
    MilestoneLocked(Id),
    /// A date was not `YYYY-MM-DD`.
    InvalidDate(String),
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
            Self::MilestoneLocked(id) => {
                write!(
                    f,
                    "milestone {id} is already invoiced and cannot change price"
                )
            }
            Self::InvalidDate(s) => write!(f, "not a YYYY-MM-DD date: {s}"),
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
    /// Unix seconds when the milestone was marked delivered. Required before invoicing.
    pub delivered_at: Option<i64>,
}

impl Milestone {
    pub fn is_invoiced(&self) -> bool {
        self.invoice_id.is_some()
    }

    pub fn is_delivered(&self) -> bool {
        self.delivered_at.is_some()
    }

    /// Returns an error if the price may not change because the milestone is invoiced.
    pub fn ensure_amount_editable(&self) -> Result<(), DomainError> {
        if self.is_invoiced() {
            Err(DomainError::MilestoneLocked(self.id))
        } else {
            Ok(())
        }
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

/// Lines for a fixed-price project: one per delivered, uninvoiced milestone.
/// Undelivered milestones are skipped, so they cannot be invoiced yet.
pub fn milestone_invoice_lines(
    project: &Project,
    milestones: &[Milestone],
) -> Result<Vec<InvoiceLine>, DomainError> {
    require_billing(project, BillingType::FixedPrice)?;

    let lines: Vec<InvoiceLine> = milestones
        .iter()
        .filter(|m| m.project_id == project.id && m.is_delivered() && !m.is_invoiced())
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

/// Date for a Unix timestamp as `YYYY-MM-DD` in UTC.
/// Uses Howard Hinnant's civil-from-days algorithm, so no date crate is needed.
pub fn iso_date_from_unix(secs: i64) -> String {
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Converts a valid `YYYY-MM-DD` date to UTC Unix seconds at midnight.
pub fn iso_date_to_unix_seconds(date: &str) -> Result<i64, DomainError> {
    days_from_civil(date)?
        .checked_mul(86_400)
        .ok_or(DomainError::Overflow)
}

/// Adds `days` to a valid `YYYY-MM-DD` date and returns the result in the same format.
/// Uses Hinnant's days-from-civil, so no date crate is needed.
pub fn add_days(date: &str, days: i64) -> Result<String, DomainError> {
    let bad = || DomainError::InvalidDate(date.to_string());
    let target = days_from_civil(date)?
        .checked_add(days)
        .ok_or(DomainError::Overflow)?;
    let seconds = target.checked_mul(86_400).ok_or(DomainError::Overflow)?;
    let result = iso_date_from_unix(seconds);
    if result.len() != 10 {
        return Err(bad());
    }
    Ok(result)
}

/// Parses and validates a Gregorian `YYYY-MM-DD` date, returning days since 1970-01-01.
fn days_from_civil(date: &str) -> Result<i64, DomainError> {
    let bad = || DomainError::InvalidDate(date.to_string());
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes[0..4].iter().all(|byte| byte.is_ascii_digit())
        || !bytes[5..7].iter().all(|byte| byte.is_ascii_digit())
        || !bytes[8..10].iter().all(|byte| byte.is_ascii_digit())
    {
        return Err(bad());
    }
    let year = date[0..4].parse::<i64>().map_err(|_| bad())?;
    let month = date[5..7].parse::<i64>().map_err(|_| bad())?;
    let day = date[8..10].parse::<i64>().map_err(|_| bad())?;
    if !(1..=12).contains(&month) {
        return Err(bad());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return Err(bad());
    }

    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let adjusted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Ok(era * 146_097 + day_of_era - 719_468)
}

/// Overdue means sent and past due. Due today is not overdue.
/// Both dates are `YYYY-MM-DD`, so string comparison orders them correctly.
pub fn is_overdue(status: InvoiceStatus, due_date: &str, today: &str) -> bool {
    status == InvoiceStatus::Sent && due_date < today
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
        assert_eq!(
            amount_for_seconds(-1, 100),
            Err(DomainError::NegativeAmount)
        );
        assert_eq!(
            amount_for_seconds(1, -100),
            Err(DomainError::NegativeAmount)
        );
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
        assert_eq!(
            hourly_invoice_lines(&p, &[]),
            Err(DomainError::NothingToInvoice)
        );
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
                delivered_at: Some(100),
            },
            Milestone {
                id: 2,
                project_id: 1,
                name: "Build".into(),
                amount_cents: 60_000,
                invoice_id: Some(7),
                delivered_at: Some(100),
            },
        ];
        let lines = milestone_invoice_lines(&p, &milestones).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].source, LineSource::Milestone(1));
        assert_eq!(lines[0].quantity_seconds, None);
    }

    #[test]
    fn undelivered_milestones_are_not_invoiced() {
        let p = project(BillingType::FixedPrice, 0);
        let milestones = vec![Milestone {
            id: 1,
            project_id: 1,
            name: "Design".into(),
            amount_cents: 40_000,
            invoice_id: None,
            delivered_at: None,
        }];
        assert_eq!(
            milestone_invoice_lines(&p, &milestones),
            Err(DomainError::NothingToInvoice)
        );
    }

    #[test]
    fn invoiced_milestone_price_is_locked() {
        let mut m = Milestone {
            id: 3,
            project_id: 1,
            name: "Build".into(),
            amount_cents: 60_000,
            invoice_id: None,
            delivered_at: Some(100),
        };
        assert!(m.ensure_amount_editable().is_ok());
        m.invoice_id = Some(9);
        assert_eq!(
            m.ensure_amount_editable(),
            Err(DomainError::MilestoneLocked(3))
        );
    }

    #[test]
    fn unix_seconds_convert_to_iso_date() {
        assert_eq!(iso_date_from_unix(0), "1970-01-01");
        assert_eq!(iso_date_from_unix(86_399), "1970-01-01");
        assert_eq!(iso_date_from_unix(86_400), "1970-01-02");
        // 2000-02-29 00:00:00 UTC, a leap day.
        assert_eq!(iso_date_from_unix(951_782_400), "2000-02-29");
        // 2026-11-01 00:00:00 UTC.
        assert_eq!(iso_date_from_unix(1_793_491_200), "2026-11-01");
    }

    #[test]
    fn overdue_only_when_sent_and_past_due() {
        use InvoiceStatus::*;
        assert!(is_overdue(Sent, "2026-10-31", "2026-11-01"));
        // Due today is not overdue.
        assert!(!is_overdue(Sent, "2026-11-01", "2026-11-01"));
        assert!(!is_overdue(Sent, "2026-11-30", "2026-11-01"));
        // Drafts and paid invoices are never overdue.
        assert!(!is_overdue(Draft, "2026-10-31", "2026-11-01"));
        assert!(!is_overdue(Paid, "2026-10-31", "2026-11-01"));
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
    fn add_days_handles_month_and_leap_boundaries() {
        assert_eq!(add_days("2026-11-01", 30).unwrap(), "2026-12-01");
        assert_eq!(add_days("2026-12-31", 1).unwrap(), "2027-01-01");
        assert_eq!(add_days("2024-02-28", 1).unwrap(), "2024-02-29");
        assert_eq!(add_days("2026-02-28", 1).unwrap(), "2026-03-01");
        assert_eq!(add_days("2026-11-01", 0).unwrap(), "2026-11-01");
        assert_eq!(add_days("2026-11-10", -9).unwrap(), "2026-11-01");
    }

    #[test]
    fn add_days_rejects_malformed_dates() {
        assert!(add_days("1 Nov 2026", 1).is_err());
        assert!(add_days("2026-13-01", 1).is_err());
        assert!(add_days("2026-11-00", 1).is_err());
        assert!(add_days("2026-02-31", 1).is_err());
        assert!(add_days("2025-02-29", 1).is_err());
        assert_eq!(add_days("2024-02-29", 1).unwrap(), "2024-03-01");
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
