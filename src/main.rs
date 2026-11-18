mod domain;
mod storage;

use std::{env, fs, path::PathBuf};

use gpui::{
    App, Application, Context, FocusHandle, InteractiveElement, KeyDownEvent, MouseButton, Render,
    Window, WindowOptions, div, prelude::*, px, rgb,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Time,
    Clients,
    Projects,
    Invoices,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    BusinessName,
    BusinessAddress,
    BusinessContact,
    Currency,
    InvoicePrefix,
    ClientName,
    ClientContact,
    ClientAddress,
    PaymentTerms,
    ProjectName,
    ProjectRate,
    TimerDescription,
    ManualDescription,
    ManualDate,
    ManualStart,
    ManualEndDate,
    ManualEnd,
    MilestoneName,
    MilestoneAmount,
    InvoiceDate,
}

struct Shell {
    db_status: String,
    data_dir: PathBuf,
    store: Option<storage::Store>,
    focus_handle: Option<FocusHandle>,
    page: Page,
    active_field: Option<Field>,
    text_cursor: usize,
    error: Option<String>,
    status: Option<String>,

    business_name: String,
    business_address: String,
    business_contact: String,
    currency: String,
    invoice_prefix: String,

    clients: Vec<storage::Client>,
    projects: Vec<domain::Project>,
    entries: Vec<domain::TimeEntry>,
    milestones: Vec<domain::Milestone>,
    invoices: Vec<storage::Invoice>,
    invoice_document: Option<storage::InvoiceDocument>,
    running_timer: Option<domain::TimeEntry>,

    selected_client_id: Option<domain::Id>,
    selected_project_id: Option<domain::Id>,
    selected_invoice_id: Option<domain::Id>,
    editing_client: Option<domain::Id>,
    editing_project: Option<domain::Id>,

    client_name: String,
    client_contact: String,
    client_address: String,
    payment_terms: String,
    project_name: String,
    project_rate: String,
    project_billing: domain::BillingType,
    timer_description: String,
    manual_description: String,
    manual_date: String,
    manual_start: String,
    manual_end_date: String,
    manual_end: String,
    manual_billable: bool,
    editing_entry: Option<domain::Id>,
    milestone_name: String,
    milestone_amount: String,
    editing_milestone: Option<domain::Id>,
    invoice_issue_date: String,
    confirm_cancel_invoice: bool,
}

fn clamp_char_boundary(value: &str, cursor: usize) -> usize {
    let mut cursor = cursor.min(value.len());
    while !value.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn previous_char_boundary(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value[..cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_char_boundary(value: &str, cursor: usize) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value[cursor..]
        .chars()
        .next()
        .map(|character| cursor + character.len_utf8())
        .unwrap_or(cursor)
}

fn insert_text_at(value: &mut String, cursor: usize, text: &str) -> usize {
    let cursor = clamp_char_boundary(value, cursor);
    value.insert_str(cursor, text);
    cursor + text.len()
}

fn backspace_at(value: &mut String, cursor: usize) -> (usize, bool) {
    let cursor = clamp_char_boundary(value, cursor);
    if cursor == 0 {
        return (cursor, false);
    }
    let previous = previous_char_boundary(value, cursor);
    value.replace_range(previous..cursor, "");
    (previous, true)
}

fn delete_at(value: &mut String, cursor: usize) -> (usize, bool) {
    let cursor = clamp_char_boundary(value, cursor);
    let next = next_char_boundary(value, cursor);
    if next == cursor {
        return (cursor, false);
    }
    value.replace_range(cursor..next, "");
    (cursor, true)
}

impl Shell {
    fn new(
        store: Option<storage::Store>,
        data_dir: PathBuf,
        db_status: String,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(store, data_dir, db_status, Some(cx.focus_handle()))
    }

    /// Builds the shell without a GPUI app context so the state machine can be
    /// driven headlessly from tests. The real entry point always supplies a handle.
    fn build(
        store: Option<storage::Store>,
        data_dir: PathBuf,
        db_status: String,
        focus_handle: Option<FocusHandle>,
    ) -> Self {
        let today = domain::iso_date_from_unix(storage::now_unix());
        let setting = |key: &str, default: &str| {
            store
                .as_ref()
                .and_then(|store| store.get_setting(key).ok().flatten())
                .unwrap_or_else(|| default.to_string())
        };
        let business_name = setting("business_name", "");
        let business_address = setting("business_address", "");
        let business_contact = setting("business_contact", "");
        let currency = setting("currency", "USD");
        let invoice_prefix = setting("invoice_prefix", "INV");
        let mut shell = Self {
            db_status,
            data_dir,
            store,
            focus_handle,
            page: if business_name.trim().is_empty() {
                Page::Settings
            } else {
                Page::Overview
            },
            active_field: None,
            text_cursor: 0,
            error: None,
            status: None,
            business_name,
            business_address,
            business_contact,
            currency,
            invoice_prefix,
            clients: Vec::new(),
            projects: Vec::new(),
            entries: Vec::new(),
            milestones: Vec::new(),
            invoices: Vec::new(),
            invoice_document: None,
            running_timer: None,
            selected_client_id: None,
            selected_project_id: None,
            selected_invoice_id: None,
            editing_client: None,
            editing_project: None,
            client_name: String::new(),
            client_contact: String::new(),
            client_address: String::new(),
            payment_terms: "30".to_string(),
            project_name: String::new(),
            project_rate: "100.00".to_string(),
            project_billing: domain::BillingType::Hourly,
            timer_description: String::new(),
            manual_description: String::new(),
            manual_date: today.clone(),
            manual_start: "09:00 AM".to_string(),
            manual_end_date: today.clone(),
            manual_end: "10:00 AM".to_string(),
            manual_billable: true,
            editing_entry: None,
            milestone_name: String::new(),
            milestone_amount: "0.00".to_string(),
            editing_milestone: None,
            invoice_issue_date: today,
            confirm_cancel_invoice: false,
        };
        shell.refresh();
        shell
    }

    fn refresh(&mut self) {
        let Some(store) = self.store.as_ref() else {
            self.clients.clear();
            self.projects.clear();
            self.entries.clear();
            self.milestones.clear();
            self.invoices.clear();
            self.invoice_document = None;
            self.running_timer = None;
            return;
        };

        let mut problems = Vec::new();
        self.clients = match store.list_clients() {
            Ok(items) => items,
            Err(error) => {
                problems.push(format!("clients: {error}"));
                Vec::new()
            }
        };
        self.projects = match store.list_projects() {
            Ok(items) => items,
            Err(error) => {
                problems.push(format!("projects: {error}"));
                Vec::new()
            }
        };
        self.invoices = match store.list_invoices(None) {
            Ok(items) => items,
            Err(error) => {
                problems.push(format!("invoices: {error}"));
                Vec::new()
            }
        };
        self.running_timer = match store.running_timer() {
            Ok(timer) => timer,
            Err(error) => {
                problems.push(format!("timer: {error}"));
                None
            }
        };

        if !self
            .selected_client_id
            .is_some_and(|id| self.clients.iter().any(|client| client.id == id))
        {
            self.selected_client_id = self.clients.first().map(|client| client.id);
        }
        if !self
            .selected_project_id
            .is_some_and(|id| self.projects.iter().any(|project| project.id == id))
        {
            self.selected_project_id = self.projects.first().map(|project| project.id);
        }
        if !self
            .selected_invoice_id
            .is_some_and(|id| self.invoices.iter().any(|invoice| invoice.id == id))
        {
            self.selected_invoice_id = self.invoices.first().map(|invoice| invoice.id);
        }

        self.entries.clear();
        self.milestones.clear();
        if let Some(project_id) = self.selected_project_id {
            match store.list_entries(project_id) {
                Ok(items) => self.entries = items,
                Err(error) => problems.push(format!("time entries: {error}")),
            }
            match store.list_milestones(project_id) {
                Ok(items) => self.milestones = items,
                Err(error) => problems.push(format!("milestones: {error}")),
            }
        }
        self.invoice_document =
            self.selected_invoice_id
                .and_then(|id| match store.invoice_document(id) {
                    Ok(document) => Some(document),
                    Err(error) => {
                        problems.push(format!("invoice details: {error}"));
                        None
                    }
                });
        self.error = if problems.is_empty() {
            None
        } else {
            Some(problems.join(" · "))
        };
    }

    /// Focuses the root element when running inside a real window. Headless
    /// builds (tests) have no window, so this is a no-op there.
    fn focus_root(&self, window: &mut Window) {
        if let Some(handle) = &self.focus_handle {
            handle.focus(window);
        }
    }

    fn field_value(&self, field: Field) -> &str {
        match field {
            Field::BusinessName => &self.business_name,
            Field::BusinessAddress => &self.business_address,
            Field::BusinessContact => &self.business_contact,
            Field::Currency => &self.currency,
            Field::InvoicePrefix => &self.invoice_prefix,
            Field::ClientName => &self.client_name,
            Field::ClientContact => &self.client_contact,
            Field::ClientAddress => &self.client_address,
            Field::PaymentTerms => &self.payment_terms,
            Field::ProjectName => &self.project_name,
            Field::ProjectRate => &self.project_rate,
            Field::TimerDescription => &self.timer_description,
            Field::ManualDescription => &self.manual_description,
            Field::ManualDate => &self.manual_date,
            Field::ManualStart => &self.manual_start,
            Field::ManualEndDate => &self.manual_end_date,
            Field::ManualEnd => &self.manual_end,
            Field::MilestoneName => &self.milestone_name,
            Field::MilestoneAmount => &self.milestone_amount,
            Field::InvoiceDate => &self.invoice_issue_date,
        }
    }

    fn field_value_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::BusinessName => &mut self.business_name,
            Field::BusinessAddress => &mut self.business_address,
            Field::BusinessContact => &mut self.business_contact,
            Field::Currency => &mut self.currency,
            Field::InvoicePrefix => &mut self.invoice_prefix,
            Field::ClientName => &mut self.client_name,
            Field::ClientContact => &mut self.client_contact,
            Field::ClientAddress => &mut self.client_address,
            Field::PaymentTerms => &mut self.payment_terms,
            Field::ProjectName => &mut self.project_name,
            Field::ProjectRate => &mut self.project_rate,
            Field::TimerDescription => &mut self.timer_description,
            Field::ManualDescription => &mut self.manual_description,
            Field::ManualDate => &mut self.manual_date,
            Field::ManualStart => &mut self.manual_start,
            Field::ManualEndDate => &mut self.manual_end_date,
            Field::ManualEnd => &mut self.manual_end,
            Field::MilestoneName => &mut self.milestone_name,
            Field::MilestoneAmount => &mut self.milestone_amount,
            Field::InvoiceDate => &mut self.invoice_issue_date,
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(field) = self.active_field else {
            return;
        };
        self.text_cursor = self.text_cursor.min(self.field_value(field).len());
        let key = event.keystroke.key.to_string().to_ascii_lowercase();
        // Paste with Cmd+V (macOS) or Ctrl+V (other platforms).
        if key == "v"
            && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
            && !event.keystroke.modifiers.alt
        {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let pasted: String = text.replace(['\n', '\r'], " ").into();
                let pasted: String =
                    pasted.chars().filter(|c| !c.is_control()).collect();
                if !pasted.is_empty() {
                    let cursor = self.text_cursor;
                    self.text_cursor =
                        insert_text_at(self.field_value_mut(field), cursor, &pasted);
                    cx.notify();
                }
            }
            return;
        }
        match key.as_str() {
            "backspace" => {
                let cursor = self.text_cursor;
                let (cursor, changed) = backspace_at(self.field_value_mut(field), cursor);
                self.text_cursor = cursor;
                if changed {
                    cx.notify();
                }
            }
            "delete" | "del" => {
                let cursor = self.text_cursor;
                let (cursor, changed) = delete_at(self.field_value_mut(field), cursor);
                self.text_cursor = cursor;
                if changed {
                    cx.notify();
                }
            }
            "left" | "arrowleft" => {
                let cursor = self.text_cursor;
                self.text_cursor = previous_char_boundary(self.field_value(field), cursor);
                cx.notify();
            }
            "right" | "arrowright" => {
                let cursor = self.text_cursor;
                self.text_cursor = next_char_boundary(self.field_value(field), cursor);
                cx.notify();
            }
            "home" => {
                self.text_cursor = 0;
                cx.notify();
            }
            "end" => {
                self.text_cursor = self.field_value(field).len();
                cx.notify();
            }
            "enter" | "tab" | "escape" => {
                self.active_field = None;
                cx.notify();
            }
            _ => {
                if let Some(text) = event.keystroke.key_char.as_ref().map(ToString::to_string) {
                    if !text.is_empty() && !text.chars().any(char::is_control) {
                        let cursor = self.text_cursor;
                        self.text_cursor =
                            insert_text_at(self.field_value_mut(field), cursor, &text);
                        cx.notify();
                    }
                }
            }
        }
    }

    fn text_field(&self, label: &str, field: Field, cx: &Context<Self>) -> impl IntoElement {
        let value = self.field_value(field).to_string();
        let has_value = !value.is_empty();
        let active = self.active_field == Some(field);
        let cursor = clamp_char_boundary(&value, self.text_cursor);
        let text = if active {
            format!("{}▏{}", &value[..cursor], &value[cursor..])
        } else if value.is_empty() {
            "Type here…".to_string()
        } else {
            value
        };
        let input = div()
            .flex_grow()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(if active { rgb(0x4d8978) } else { rgb(0xd9e1dd) })
            .bg(if active { rgb(0xffffff) } else { rgb(0xf7f9f7) })
            .text_color(if has_value || active {
                rgb(0x203a34)
            } else {
                rgb(0x7d8d87)
            })
            .cursor_text()
            .child(text)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.active_field = Some(field);
                    this.text_cursor = this.field_value(field).len();
                    this.focus_root(window);
                    cx.notify();
                }),
            );
        let mut controls = div().flex().gap_2().child(input);
        if has_value {
            controls = controls.child(div().p_2().bg(rgb(0xe7e5e4)).child("Clear").on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.field_value_mut(field).clear();
                    this.active_field = Some(field);
                    this.text_cursor = 0;
                    this.focus_root(window);
                    cx.notify();
                }),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label.to_string())
            .child(controls)
    }

    fn navigation_button(
        &self,
        page: Page,
        icon: &'static str,
        label: &'static str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let active = self.page == page;
        div()
            .w_full()
            .p_3()
            .rounded_md()
            .bg(if active { rgb(0x254b42) } else { rgb(0x142b28) })
            .text_color(if active { rgb(0xffffff) } else { rgb(0xb8c9c4) })
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().w(px(22.)).child(icon))
                    .child(label),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.page = page;
                    this.active_field = None;
                    this.confirm_cancel_invoice = false;
                    this.focus_root(window);
                    cx.notify();
                }),
            )
    }

    fn page_heading(&self) -> (&'static str, &'static str) {
        match self.page {
            Page::Overview => (
                "Dashboard",
                "A clear view of your clients, work, and billing.",
            ),
            Page::Time => ("Time tracking", "Track live work or add time manually."),
            Page::Clients => (
                "Clients",
                "Keep client details and payment terms in one place.",
            ),
            Page::Projects => (
                "Projects",
                "Organize billable work and manage project milestones.",
            ),
            Page::Invoices => (
                "Invoices",
                "Create drafts, review line items, and track payment status.",
            ),
            Page::Settings => (
                "Settings",
                "Set up your business details and invoice defaults.",
            ),
        }
    }

    fn action_button(
        &self,
        page: Page,
        label: &'static str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .p_3()
            .rounded_md()
            .bg(rgb(0x254b42))
            .text_color(rgb(0xffffff))
            .cursor_pointer()
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.page = page;
                    this.active_field = None;
                    this.focus_root(window);
                    cx.notify();
                }),
            )
    }

    fn project_button(
        &self,
        id: domain::Id,
        label: String,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let active = self.selected_project_id == Some(id);
        div()
            .p_2()
            .bg(if active { rgb(0xd6d3d1) } else { rgb(0xe7e5e4) })
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let client_id = this
                        .projects
                        .iter()
                        .find(|project| project.id == id)
                        .map(|project| project.client_id);
                    this.selected_project_id = Some(id);
                    this.selected_client_id = client_id;
                    this.refresh();
                    cx.notify();
                }),
            )
    }

    fn client_label(&self, id: domain::Id) -> String {
        self.clients
            .iter()
            .find(|client| client.id == id)
            .map(|client| client.name.clone())
            .unwrap_or_else(|| format!("Client {id}"))
    }

    fn project_label(&self, id: domain::Id) -> String {
        self.projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.name.clone())
            .unwrap_or_else(|| format!("Project {id}"))
    }

    fn save_settings(&mut self) {
        let business_name = self.business_name.trim().to_string();
        let currency = self.currency.trim().to_ascii_uppercase();
        let prefix = self.invoice_prefix.trim().to_ascii_uppercase();
        if business_name.is_empty() {
            self.error = Some("Enter your business or trading name before saving.".to_string());
            return;
        }
        if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
            self.error =
                Some("Currency must be a three-letter code, such as USD or EUR.".to_string());
            return;
        }
        if prefix.is_empty()
            || prefix.len() > 12
            || !prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            self.error =
                Some("Invoice prefix must be 1–12 letters, digits, or hyphens.".to_string());
            return;
        }
        let Some(store) = self.store.as_ref() else {
            self.error = Some(self.db_status.clone());
            return;
        };
        let result = store
            .set_setting("business_name", &business_name)
            .and_then(|_| store.set_setting("business_address", self.business_address.trim()))
            .and_then(|_| store.set_setting("business_contact", self.business_contact.trim()))
            .and_then(|_| store.set_setting("currency", &currency))
            .and_then(|_| store.set_setting("invoice_prefix", &prefix));
        match result {
            Ok(()) => {
                self.business_name = business_name;
                self.currency = currency;
                self.invoice_prefix = prefix;
                self.page = Page::Overview;
                self.active_field = None;
                self.error = None;
                self.status = Some("Business and invoice settings saved.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn save_client(&mut self) {
        let name = self.client_name.trim().to_string();
        if name.is_empty() {
            self.error = Some("Client name is required.".to_string());
            return;
        }
        let terms = match self.payment_terms.trim().parse::<i64>() {
            Ok(days) if (0..=3650).contains(&days) => days,
            _ => {
                self.error =
                    Some("Payment terms must be a whole number from 0 to 3650 days.".to_string());
                return;
            }
        };
        let Some(store) = self.store.as_ref() else {
            self.error = Some(self.db_status.clone());
            return;
        };
        let editing = self.editing_client;
        let result = if let Some(id) = editing {
            store
                .update_client(
                    id,
                    &name,
                    self.client_contact.trim(),
                    self.client_address.trim(),
                    terms,
                )
                .map(|()| id)
                .map_err(|error| error.to_string())
        } else {
            store
                .create_client(
                    &name,
                    self.client_contact.trim(),
                    self.client_address.trim(),
                )
                .and_then(|id| {
                    store.set_client_payment_terms(id, terms)?;
                    Ok(id)
                })
                .map_err(|error| error.to_string())
        };
        match result {
            Ok(id) => {
                self.editing_client = None;
                self.selected_client_id = Some(id);
                self.client_name.clear();
                self.client_contact.clear();
                self.client_address.clear();
                self.payment_terms = "30".to_string();
                self.active_field = None;
                self.refresh();
                self.selected_client_id = Some(id);
                self.error = None;
                self.status = Some(if editing.is_some() {
                    "Client updated.".to_string()
                } else {
                    "Client created.".to_string()
                });
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn begin_edit_client(&mut self, id: domain::Id) {
        if let Some(client) = self.clients.iter().find(|client| client.id == id).cloned() {
            self.editing_client = Some(id);
            self.selected_client_id = Some(id);
            self.client_name = client.name;
            self.client_contact = client.contact;
            self.client_address = client.address;
            self.payment_terms = client.payment_terms_days.to_string();
            self.active_field = None;
            self.error = None;
        }
    }

    fn cancel_client_edit(&mut self) {
        self.editing_client = None;
        self.client_name.clear();
        self.client_contact.clear();
        self.client_address.clear();
        self.payment_terms = "30".to_string();
        self.active_field = None;
        self.error = None;
    }

    fn create_project(&mut self) {
        let Some(client_id) = self.selected_client_id else {
            self.error = Some("Create and select a client before adding a project.".to_string());
            return;
        };
        let name = self.project_name.trim().to_string();
        if name.is_empty() {
            self.error = Some("Project name is required.".to_string());
            return;
        }
        let rate_cents = match self.project_billing {
            domain::BillingType::Hourly => match parse_money_cents(&self.project_rate) {
                Some(amount) => amount,
                None => {
                    self.error =
                        Some("Enter a non-negative hourly rate, such as 100.00.".to_string());
                    return;
                }
            },
            domain::BillingType::FixedPrice => 0,
        };
        let result = match self.store.as_ref() {
            Some(store) => store.create_project(client_id, &name, self.project_billing, rate_cents),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(id) => {
                self.project_name.clear();
                self.selected_project_id = Some(id);
                self.refresh();
                self.selected_project_id = Some(id);
                self.error = None;
                self.status = Some("Project created.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn save_project(&mut self) {
        if self.editing_project.is_some() {
            self.update_project();
        } else {
            self.create_project();
        }
    }

    fn begin_edit_project(&mut self, id: domain::Id) {
        if let Some(project) = self.projects.iter().find(|item| item.id == id).cloned() {
            self.editing_project = Some(id);
            self.selected_project_id = Some(id);
            self.selected_client_id = Some(project.client_id);
            self.project_name = project.name;
            self.project_rate = format_amount_input(project.hourly_rate_cents);
            self.project_billing = project.billing;
            self.active_field = None;
            self.error = None;
        }
    }

    fn update_project(&mut self) {
        let Some(id) = self.editing_project else {
            return;
        };
        let Some(project) = self.projects.iter().find(|item| item.id == id) else {
            self.error = Some("Selected project is no longer available.".to_string());
            return;
        };
        let name = self.project_name.trim().to_string();
        if name.is_empty() {
            self.error = Some("Project name is required.".to_string());
            return;
        }
        let rate_cents = match project.billing {
            domain::BillingType::Hourly => match parse_money_cents(&self.project_rate) {
                Some(amount) => amount,
                None => {
                    self.error =
                        Some("Enter a non-negative hourly rate, such as 100.00.".to_string());
                    return;
                }
            },
            domain::BillingType::FixedPrice => 0,
        };
        let result = match self.store.as_ref() {
            Some(store) => store.update_project_details(id, &name, rate_cents),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(()) => {
                self.editing_project = None;
                self.project_name.clear();
                self.project_rate = "100.00".to_string();
                self.project_billing = domain::BillingType::Hourly;
                self.active_field = None;
                self.refresh();
                self.selected_project_id = Some(id);
                self.error = None;
                self.status = Some("Project details updated.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn cancel_project_edit(&mut self) {
        self.editing_project = None;
        self.project_name.clear();
        self.project_rate = "100.00".to_string();
        self.project_billing = domain::BillingType::Hourly;
        self.active_field = None;
        self.error = None;
    }

    fn save_milestone(&mut self) {
        let Some(project_id) = self.selected_project_id else {
            self.error = Some("Select a project first.".to_string());
            return;
        };
        let Some(project) = self
            .projects
            .iter()
            .find(|project| project.id == project_id)
        else {
            self.error = Some("Selected project is no longer available.".to_string());
            return;
        };
        if project.billing != domain::BillingType::FixedPrice {
            self.error = Some("Milestones are for fixed-price projects.".to_string());
            return;
        }
        let amount = match parse_money_cents(&self.milestone_amount) {
            Some(amount) => amount,
            None => {
                self.error =
                    Some("Enter a non-negative milestone amount, such as 500.00.".to_string());
                return;
            }
        };
        let result = if let Some(id) = self.editing_milestone {
            self.store
                .as_ref()
                .ok_or_else(|| "Database is unavailable.".to_string())
                .and_then(|store| {
                    store
                        .update_milestone_amount(id, amount)
                        .map_err(|e| e.to_string())
                })
                .map(|_| id)
        } else {
            let name = self.milestone_name.trim().to_string();
            if name.is_empty() {
                self.error = Some("Milestone name is required.".to_string());
                return;
            }
            self.store
                .as_ref()
                .ok_or_else(|| "Database is unavailable.".to_string())
                .and_then(|store| {
                    store
                        .add_milestone(project_id, &name, amount)
                        .map_err(|e| e.to_string())
                })
        };
        match result {
            Ok(_) => {
                let was_edit = self.editing_milestone.is_some();
                self.editing_milestone = None;
                self.milestone_name.clear();
                self.milestone_amount = "0.00".to_string();
                self.refresh();
                self.error = None;
                self.status = Some(if was_edit {
                    "Milestone amount updated.".to_string()
                } else {
                    "Milestone created.".to_string()
                });
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn begin_edit_milestone(&mut self, id: domain::Id) {
        if let Some(milestone) = self.milestones.iter().find(|item| item.id == id).cloned() {
            self.editing_milestone = Some(id);
            self.milestone_name = milestone.name;
            self.milestone_amount = format_amount_input(milestone.amount_cents);
            self.error = None;
        }
    }

    fn mark_milestone_delivered(&mut self, id: domain::Id) {
        let result = match self.store.as_ref() {
            Some(store) => store.mark_milestone_delivered(id, storage::now_unix()),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(()) => {
                self.refresh();
                self.error = None;
                self.status = Some("Milestone marked delivered.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn start_timer(&mut self) {
        let Some(project_id) = self.selected_project_id else {
            self.error = Some("Select a project before starting a timer.".to_string());
            return;
        };
        let result = match self.store.as_ref() {
            Some(store) => store.start_timer(
                project_id,
                self.timer_description.trim(),
                storage::now_unix(),
            ),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(_) => {
                self.timer_description.clear();
                self.refresh();
                self.error = None;
                self.status = Some("Timer started.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn stop_timer(&mut self) {
        let result = match self.store.as_ref() {
            Some(store) => store.stop_timer(storage::now_unix()),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(_) => {
                self.refresh();
                self.error = None;
                self.status = Some("Timer stopped and time saved.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn save_manual_entry(&mut self) {
        let start = match parse_utc_datetime(&self.manual_date, &self.manual_start) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let end = match parse_utc_datetime(&self.manual_end_date, &self.manual_end) {
            Ok(timestamp) => timestamp,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let Some(project_id) = self.selected_project_id else {
            self.error = Some("Select a project before adding time.".to_string());
            return;
        };
        let description = self.manual_description.trim().to_string();
        let result = match self.store.as_ref() {
            Some(store) => match self.editing_entry {
                Some(id) => store
                    .update_entry(id, &description, start, end, self.manual_billable)
                    .map(|_| id),
                None => store.add_manual_entry(
                    project_id,
                    &description,
                    start,
                    end,
                    self.manual_billable,
                ),
            },
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(_) => {
                let was_edit = self.editing_entry.is_some();
                self.editing_entry = None;
                self.manual_description.clear();
                let today = domain::iso_date_from_unix(storage::now_unix());
                self.manual_date = today.clone();
                self.manual_start = "09:00 AM".to_string();
                self.manual_end_date = today;
                self.manual_end = "10:00 AM".to_string();
                self.manual_billable = true;
                self.refresh();
                self.error = None;
                self.status = Some(if was_edit {
                    "Time entry updated.".to_string()
                } else {
                    "Manual time entry added.".to_string()
                });
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn begin_edit_entry(&mut self, id: domain::Id) {
        if let Some(entry) = self.entries.iter().find(|item| item.id == id).cloned() {
            if entry.is_running() || entry.is_invoiced() {
                return;
            }
            let end = entry.end_at.unwrap_or(entry.start_at);
            self.editing_entry = Some(id);
            self.manual_description = entry.description;
            self.manual_date = domain::iso_date_from_unix(entry.start_at);
            self.manual_start = format_time_utc(entry.start_at);
            self.manual_end_date = domain::iso_date_from_unix(end);
            self.manual_end = format_time_utc(end);
            self.manual_billable = entry.billable;
            self.error = None;
        }
    }

    fn create_invoice(&mut self) {
        if self.business_name.trim().is_empty() {
            self.page = Page::Settings;
            self.error =
                Some("Add your business name in Settings before creating invoices.".to_string());
            return;
        }
        if let Err(error) = domain::add_days(&self.invoice_issue_date, 0) {
            self.error = Some(error.to_string());
            return;
        }
        let Some(project_id) = self.selected_project_id else {
            self.error = Some("Select a project before creating an invoice.".to_string());
            return;
        };
        let result = match self.store.as_mut() {
            Some(store) => store.create_invoice(project_id, &self.invoice_issue_date),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(id) => {
                self.selected_invoice_id = Some(id);
                self.refresh();
                self.selected_invoice_id = Some(id);
                self.error = None;
                self.status = Some("Draft created. Its billable items are reserved until the draft is sent or cancelled.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn mark_invoice_sent(&mut self) {
        let Some(id) = self.selected_invoice_id else {
            self.error = Some("Select an invoice first.".to_string());
            return;
        };
        let result = match self.store.as_mut() {
            Some(store) => store.send_invoice(id, storage::now_unix()),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(number) => {
                self.refresh();
                self.error = None;
                self.status = Some(format!("Invoice {number} marked sent. No email was sent."));
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn mark_invoice_paid(&mut self) {
        let Some(id) = self.selected_invoice_id else {
            self.error = Some("Select an invoice first.".to_string());
            return;
        };
        let result = match self.store.as_ref() {
            Some(store) => store.set_invoice_status(id, domain::InvoiceStatus::Paid),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(()) => {
                self.refresh();
                self.error = None;
                self.status = Some("Invoice marked paid.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn cancel_invoice_draft(&mut self) {
        let Some(id) = self.selected_invoice_id else {
            self.error = Some("Select an invoice first.".to_string());
            return;
        };
        let result = match self.store.as_mut() {
            Some(store) => store.cancel_invoice_draft(id),
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        match result {
            Ok(()) => {
                self.confirm_cancel_invoice = false;
                self.selected_invoice_id = None;
                self.refresh();
                self.error = None;
                self.status = Some("Draft cancelled; its time entries or milestones are available to invoice again.".to_string());
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn export_invoice(&mut self) {
        let Some(id) = self.selected_invoice_id else {
            self.error = Some("Select an invoice first.".to_string());
            return;
        };
        let document = match self.store.as_ref() {
            Some(store) => match store.invoice_document(id) {
                Ok(document) => document,
                Err(error) => {
                    self.error = Some(error.to_string());
                    return;
                }
            },
            None => {
                self.error = Some(self.db_status.clone());
                return;
            }
        };
        let directory = self.data_dir.join("Invoices");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.error = Some(format!("Could not create export folder: {error}"));
            return;
        }
        let filename_source = document
            .invoice
            .number
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("draft-{}", document.invoice.id));
        let filename = safe_filename(&filename_source);
        let path = directory.join(format!("{filename}.html"));
        match fs::write(&path, invoice_html(&document)) {
            Ok(()) => {
                self.error = None;
                self.status = Some(format!(
                    "Invoice HTML exported to {}. Open it in a browser and print/save as PDF.",
                    path.display()
                ));
            }
            Err(error) => self.error = Some(format!("Could not export invoice: {error}")),
        }
    }

    fn overview_content(&self, cx: &Context<Self>) -> impl IntoElement {
        let business = if self.business_name.trim().is_empty() {
            "Welcome to Pockets".to_string()
        } else {
            format!("Welcome back, {}", self.business_name.trim())
        };
        let outstanding_cents = self
            .invoices
            .iter()
            .filter(|invoice| invoice.status != domain::InvoiceStatus::Paid)
            .map(|invoice| invoice.total_cents)
            .fold(0_i64, i64::saturating_add);

        let metrics = div()
            .flex()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_grow()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .child("Clients")
                    .child(
                        div()
                            .text_size(px(24.))
                            .text_color(rgb(0x183630))
                            .child(self.clients.len().to_string()),
                    )
                    .child("Your client records"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_grow()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .child("Projects")
                    .child(
                        div()
                            .text_size(px(24.))
                            .text_color(rgb(0x183630))
                            .child(self.projects.len().to_string()),
                    )
                    .child("Work in progress"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_grow()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .child("Outstanding")
                    .child(
                        div()
                            .text_size(px(24.))
                            .text_color(rgb(0x183630))
                            .child(format_money(outstanding_cents, &self.currency)),
                    )
                    .child("Draft and sent invoices"),
            );

        let timer_summary = if let Some(timer) = &self.running_timer {
            format!(
                "{} · {} · started {} UTC",
                self.project_label(timer.project_id),
                display_description(&timer.description),
                format_timestamp_utc(timer.start_at)
            )
        } else {
            "No timer is running. Start tracking time against a project.".to_string()
        };
        let mut quick_actions = div().flex().gap_3();
        quick_actions = quick_actions
            .child(self.action_button(Page::Time, "Track time", cx))
            .child(self.action_button(Page::Clients, "Manage clients", cx))
            .child(self.action_button(Page::Projects, "Manage projects", cx))
            .child(self.action_button(Page::Invoices, "View invoices", cx));

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(0xe3eee9))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(22.))
                            .text_color(rgb(0x183630))
                            .child(business),
                    )
                    .child(if self.business_name.trim().is_empty() {
                        "Add your business details to prepare your first invoice."
                    } else {
                        "Your freelance workspace is ready. Pick up where you left off."
                    }),
            )
            .child(metrics)
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Quick access")
                    .child(quick_actions),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(if self.running_timer.is_some() {
                        "Timer in progress"
                    } else {
                        "Time tracking"
                    })
                    .child(timer_summary)
                    .child(self.action_button(Page::Time, "Open time tracking", cx)),
            )
    }

    fn clients_projects_content(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut client_rows = div().flex().flex_col().gap_2();
        for client in &self.clients {
            let id = client.id;
            let selected = self.selected_client_id == Some(id);
            let label = format!("{}{}", if selected { "✓  " } else { "" }, client.name);
            let select_button = div()
                .p_2()
                .bg(if selected {
                    rgb(0xd6d3d1)
                } else {
                    rgb(0xe7e5e4)
                })
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_client_id = Some(id);
                        cx.notify();
                    }),
                );
            let edit_button = div().p_2().bg(rgb(0xe7e5e4)).child("Edit").on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.begin_edit_client(id);
                    cx.notify();
                }),
            );
            client_rows =
                client_rows.child(div().flex().gap_2().child(select_button).child(edit_button));
        }

        let mut client_form = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.text_field("Client name *", Field::ClientName, cx))
            .child(self.text_field("Contact (email or phone)", Field::ClientContact, cx))
            .child(self.text_field("Billing address", Field::ClientAddress, cx))
            .child(self.text_field("Payment terms (days)", Field::PaymentTerms, cx))
            .child(
                div()
                    .p_2()
                    .bg(rgb(0xd6d3d1))
                    .child(if self.editing_client.is_some() {
                        "Save client changes"
                    } else {
                        "Create client"
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.save_client();
                            cx.notify();
                        }),
                    ),
            );
        if self.editing_client.is_some() {
            client_form = client_form.child(
                div()
                    .p_2()
                    .bg(rgb(0xe7e5e4))
                    .child("Cancel client edit")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.cancel_client_edit();
                            cx.notify();
                        }),
                    ),
            );
        }

        let mut project_rows = div().flex().flex_col().gap_2();
        for project in &self.projects {
            let id = project.id;
            let selected = self.selected_project_id == Some(id);
            let billing = match project.billing {
                domain::BillingType::Hourly => format!(
                    "hourly at {} / hour",
                    format_money(project.hourly_rate_cents, &self.currency)
                ),
                domain::BillingType::FixedPrice => "fixed price".to_string(),
            };
            let label = format!(
                "{}{} · {} · {}",
                if selected { "✓  " } else { "" },
                project.name,
                self.client_label(project.client_id),
                billing
            );
            let mut row = div()
                .flex()
                .gap_2()
                .child(self.project_button(id, label, cx));
            if selected {
                row = row.child(div().p_2().bg(rgb(0xe7e5e4)).child("Edit").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.begin_edit_project(id);
                        cx.notify();
                    }),
                ));
            }
            project_rows = project_rows.child(row);
        }

        let mut client_picker = div().flex().flex_col().gap_2();
        for client in &self.clients {
            let id = client.id;
            let selected = self.selected_client_id == Some(id);
            client_picker = client_picker.child(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(if selected {
                        rgb(0xdce9e5)
                    } else {
                        rgb(0xf1f5f3)
                    })
                    .text_color(rgb(0x24463e))
                    .child(client.name.clone())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_client_id = Some(id);
                            cx.notify();
                        }),
                    ),
            );
        }

        let selected_client = self
            .selected_client_id
            .map(|id| self.client_label(id))
            .unwrap_or_else(|| "No client selected".to_string());
        let billing_label = match (self.project_billing, self.editing_project.is_some()) {
            (domain::BillingType::Hourly, false) => {
                "Billing type: hourly (click to switch to fixed price)"
            }
            (domain::BillingType::FixedPrice, false) => {
                "Billing type: fixed price (click to switch to hourly)"
            }
            (domain::BillingType::Hourly, true) => "Billing type: hourly (locked while editing)",
            (domain::BillingType::FixedPrice, true) => {
                "Billing type: fixed price (locked while editing)"
            }
        };
        let mut project_form = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(format!("For client: {selected_client}"))
            .child(self.text_field("Project name *", Field::ProjectName, cx))
            .child(
                div()
                    .p_2()
                    .bg(rgb(0xe7e5e4))
                    .child(billing_label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if this.editing_project.is_none() {
                                this.project_billing = match this.project_billing {
                                    domain::BillingType::Hourly => domain::BillingType::FixedPrice,
                                    domain::BillingType::FixedPrice => domain::BillingType::Hourly,
                                };
                            }
                            cx.notify();
                        }),
                    ),
            );
        if self.project_billing == domain::BillingType::Hourly {
            project_form = project_form.child(self.text_field(
                "Hourly rate per hour (e.g. 100.00)",
                Field::ProjectRate,
                cx,
            ));
        } else {
            project_form = project_form.child(
                div().child("Fixed-price projects are billed through delivered milestones."),
            );
        }
        project_form = project_form.child(
            div()
                .p_2()
                .bg(rgb(0xd6d3d1))
                .child(if self.editing_project.is_some() {
                    "Save project changes"
                } else {
                    "Create project"
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.save_project();
                        cx.notify();
                    }),
                ),
        );
        if self.editing_project.is_some() {
            project_form = project_form.child(
                div()
                    .p_2()
                    .bg(rgb(0xe7e5e4))
                    .child("Cancel project edit")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.cancel_project_edit();
                            cx.notify();
                        }),
                    ),
            );
            project_form = project_form.child(
                "Billing type is locked while editing. Renaming updates project labels in invoice history; recorded totals stay fixed.",
            );
        }

        let milestone_panel = if let Some(project) = self
            .selected_project_id
            .and_then(|id| self.projects.iter().find(|project| project.id == id))
        {
            if project.billing == domain::BillingType::FixedPrice {
                let mut rows = div().flex().flex_col().gap_2();
                for milestone in &self.milestones {
                    let id = milestone.id;
                    let state = if milestone.invoice_id.is_some() {
                        "invoiced"
                    } else if milestone.is_delivered() {
                        "delivered · ready to invoice"
                    } else {
                        "not delivered"
                    };
                    let mut row = div().flex().gap_2().child(format!(
                        "{} · {} · {}",
                        milestone.name,
                        format_money(milestone.amount_cents, &self.currency),
                        state
                    ));
                    if milestone.invoice_id.is_none() && !milestone.is_delivered() {
                        row = row.child(
                            div()
                                .p_2()
                                .bg(rgb(0xe7e5e4))
                                .child("Mark delivered")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.mark_milestone_delivered(id);
                                        cx.notify();
                                    }),
                                ),
                        );
                    }
                    if milestone.invoice_id.is_none() {
                        row = row.child(
                            div()
                                .p_2()
                                .bg(rgb(0xe7e5e4))
                                .child("Edit amount")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.begin_edit_milestone(id);
                                        cx.notify();
                                    }),
                                ),
                        );
                    }
                    rows = rows.child(row);
                }
                let save_label = if self.editing_milestone.is_some() {
                    "Save milestone amount"
                } else {
                    "Add milestone"
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child("Fixed-price milestones")
                    .child(rows)
                    .child(self.text_field("Milestone name", Field::MilestoneName, cx))
                    .child(self.text_field("Milestone amount", Field::MilestoneAmount, cx))
                    .child(
                        "Editing an existing milestone changes its amount; its name is unchanged.",
                    )
                    .child(
                        div()
                            .p_2()
                            .bg(rgb(0xd6d3d1))
                            .child(save_label)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.save_milestone();
                                    cx.notify();
                                }),
                            ),
                    )
            } else {
                div().child("Hourly projects bill from completed billable time entries.")
            }
        } else {
            div().child("Create or select a project to manage it.")
        };

        if self.page == Page::Projects {
            let project_list = if self.projects.is_empty() {
                div().child("No projects yet. Choose a client and create your first project below.")
            } else {
                project_rows
            };
            let client_picker = if self.clients.is_empty() {
                div().child("Create a client first from the Clients screen.")
            } else {
                client_picker
            };
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0xffffff))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child("Choose a client")
                        .child(client_picker),
                )
                .child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0xffffff))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child("Project list")
                        .child(project_list),
                )
                .child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0xffffff))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(if self.editing_project.is_some() {
                            "Edit project"
                        } else {
                            "Create a project"
                        })
                        .child(project_form),
                )
                .child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0xffffff))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(milestone_panel),
                )
        } else {
            let client_list = if self.clients.is_empty() {
                div().child("No clients yet. Add your first client using the form below.")
            } else {
                client_rows
            };
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0xffffff))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child("Your clients")
                        .child(client_list),
                )
                .child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0xffffff))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(if self.editing_client.is_some() {
                            "Edit client details"
                        } else {
                            "Add a client"
                        })
                        .child(client_form)
                        .child("Click a field and type. Use ←/→, Home/End, Backspace, or Delete to edit; paste with Cmd/Ctrl+V."),
                )
        }
    }

    fn time_content(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut project_rows = div().flex().flex_col().gap_2();
        for project in &self.projects {
            let label = format!(
                "{} · {}",
                project.name,
                self.client_label(project.client_id)
            );
            project_rows = project_rows.child(self.project_button(project.id, label, cx));
        }
        let timer_panel = if let Some(timer) = &self.running_timer {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(format!(
                    "Timer running on {} · {} · started {} UTC · elapsed {}",
                    self.project_label(timer.project_id),
                    display_description(&timer.description),
                    format_timestamp_utc(timer.start_at),
                    format_duration(storage::now_unix() - timer.start_at)
                ))
                .child("The elapsed display updates when you refresh or take an action.")
                .child(
                    div()
                        .p_2()
                        .bg(rgb(0xd6d3d1))
                        .child("Stop timer")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.stop_timer();
                                cx.notify();
                            }),
                        ),
                )
        } else {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(self.text_field("What are you working on?", Field::TimerDescription, cx))
                .child(
                    div()
                        .p_2()
                        .bg(rgb(0xd6d3d1))
                        .child("Start timer")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.start_timer();
                                cx.notify();
                            }),
                        ),
                )
        };

        let billable_label = if self.manual_billable {
            "Manual entry: billable (click to make non-billable)"
        } else {
            "Manual entry: non-billable (click to make billable)"
        };
        let manual_form = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.text_field("Description", Field::ManualDescription, cx))
            .child(self.text_field("Start date (YYYY-MM-DD, UTC)", Field::ManualDate, cx))
            .child(self.text_field("Start time (HH:MM AM/PM, UTC)", Field::ManualStart, cx))
            .child(self.text_field("End date (YYYY-MM-DD, UTC)", Field::ManualEndDate, cx))
            .child(self.text_field("End time (HH:MM AM/PM, UTC)", Field::ManualEnd, cx))
            .child(
                div()
                    .p_2()
                    .bg(rgb(0xe7e5e4))
                    .child(billable_label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.manual_billable = !this.manual_billable;
                            cx.notify();
                        }),
                    ),
            )
            .child(
                div()
                    .p_2()
                    .bg(rgb(0xd6d3d1))
                    .child(if self.editing_entry.is_some() {
                        "Save time entry changes"
                    } else {
                        "Add manual time"
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.save_manual_entry();
                            cx.notify();
                        }),
                    ),
            );

        let mut entry_rows = div().flex().flex_col().gap_2();
        for entry in &self.entries {
            let duration = entry
                .duration_seconds()
                .map(format_duration)
                .unwrap_or_else(|| "running".to_string());
            let invoiced = if entry.is_invoiced() {
                format!("invoiced #{}", entry.invoice_id.unwrap_or_default())
            } else if entry.billable {
                "billable · unbilled".to_string()
            } else {
                "non-billable".to_string()
            };
            let mut row = div().flex().gap_2().child(format!(
                "{} · {} · {} · {}",
                display_description(&entry.description),
                format_timestamp_utc(entry.start_at),
                duration,
                invoiced
            ));
            if !entry.is_running() && !entry.is_invoiced() {
                let id = entry.id;
                row = row.child(div().p_2().bg(rgb(0xe7e5e4)).child("Edit").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.begin_edit_entry(id);
                        cx.notify();
                    }),
                ));
            }
            entry_rows = entry_rows.child(row);
        }

        let project_selection = if self.projects.is_empty() {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child("Create a project before recording time.")
                .child(self.action_button(Page::Projects, "Go to projects", cx))
        } else {
            project_rows
        };

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Choose a project")
                    .child(project_selection)
                    .child("Time is stored in UTC."),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xe3eee9))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(if self.running_timer.is_some() {
                        "Running timer"
                    } else {
                        "Start a timer"
                    })
                    .child(timer_panel),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Add manual time")
                    .child(manual_form),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(format!("Time entries · {}", self.entries.len()))
                    .child(if self.entries.is_empty() {
                        "No time entries for this project yet.".to_string()
                    } else {
                        "History for the selected project".to_string()
                    })
                    .child(entry_rows)
                    .child("Invoiced entries are locked. Cancel a draft invoice to release its entries.")
                    .child(
                        div()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(0xe7eeeb))
                            .child("Refresh data")
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                this.refresh();
                                cx.notify();
                            })),
                    ),
            )
    }

    fn invoices_content(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut project_rows = div().flex().flex_col().gap_2();
        for project in &self.projects {
            project_rows = project_rows.child(self.project_button(
                project.id,
                format!(
                    "{} · {}",
                    project.name,
                    self.client_label(project.client_id)
                ),
                cx,
            ));
        }
        let create_panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .child("Create a draft from unbilled work for the selected project.")
            .child(self.text_field("Issue date (YYYY-MM-DD)", Field::InvoiceDate, cx))
            .child(
                div()
                    .p_2()
                    .bg(rgb(0xd6d3d1))
                    .child("Create invoice draft")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.create_invoice();
                            cx.notify();
                        }),
                    ),
            );

        let mut invoice_rows = div().flex().flex_col().gap_2();
        for invoice in &self.invoices {
            let id = invoice.id;
            let selected = self.selected_invoice_id == Some(id);
            let label = format!(
                "{} · {} · {} · {} · due {}",
                invoice
                    .number
                    .clone()
                    .unwrap_or_else(|| format!("Draft {}", id)),
                self.project_label(invoice.project_id),
                format!("{:?}", invoice.status),
                format_money(invoice.total_cents, &invoice.currency),
                invoice.due_date
            );
            invoice_rows = invoice_rows.child(
                div()
                    .p_2()
                    .bg(if selected {
                        rgb(0xd6d3d1)
                    } else {
                        rgb(0xe7e5e4)
                    })
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.selected_invoice_id = Some(id);
                            this.confirm_cancel_invoice = false;
                            this.refresh();
                            cx.notify();
                        }),
                    ),
            );
        }

        let detail = if let Some(document) = &self.invoice_document {
            let invoice = &document.invoice;
            let mut lines = div().flex().flex_col().gap_2();
            for line in &document.lines {
                let quantity = line
                    .quantity_seconds
                    .map(format_duration)
                    .unwrap_or_else(|| "—".to_string());
                lines = lines.child(format!(
                    "{} · {} · {}",
                    display_description(&line.description),
                    quantity,
                    format_money(line.amount_cents, &document.currency)
                ));
            }
            let mut actions = div().flex().flex_col().gap_2();
            actions = actions.child(
                div()
                    .p_2()
                    .bg(rgb(0xd6d3d1))
                    .child("Export invoice HTML")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.export_invoice();
                            cx.notify();
                        }),
                    ),
            );
            match invoice.status {
                domain::InvoiceStatus::Draft => {
                    actions = actions.child(
                        div()
                            .p_2()
                            .bg(rgb(0xe7e5e4))
                            .child("Assign number & mark sent (does not email)")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.mark_invoice_sent();
                                    cx.notify();
                                }),
                            ),
                    );
                    if self.confirm_cancel_invoice {
                        actions = actions
                            .child(
                                div()
                                    .p_2()
                                    .bg(rgb(0xe7e5e4))
                                    .child("Confirm cancel draft and release items")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.cancel_invoice_draft();
                                            cx.notify();
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .p_2()
                                    .bg(rgb(0xe7e5e4))
                                    .child("Keep draft")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.confirm_cancel_invoice = false;
                                            cx.notify();
                                        }),
                                    ),
                            );
                    } else {
                        actions = actions.child(
                            div()
                                .p_2()
                                .bg(rgb(0xe7e5e4))
                                .child("Cancel draft…")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.confirm_cancel_invoice = true;
                                        cx.notify();
                                    }),
                                ),
                        );
                    }
                }
                domain::InvoiceStatus::Sent => {
                    actions = actions.child(
                        div()
                            .p_2()
                            .bg(rgb(0xe7e5e4))
                            .child("Mark paid")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.mark_invoice_paid();
                                    cx.notify();
                                }),
                            ),
                    );
                }
                domain::InvoiceStatus::Paid => {}
            }
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(format!(
                    "Invoice {} · {:?}",
                    invoice.number.clone().unwrap_or_else(|| format!("Draft {}", invoice.id)),
                    invoice.status
                ))
                .child(format!("Client: {}", document.client.name))
                .child(format!("Project: {}", document.project.name))
                .child(format!("Issued {} · due {}", invoice.issue_date, invoice.due_date))
                .child(lines)
                .child(format!(
                    "Total: {}",
                    format_money(invoice.total_cents, &document.currency)
                ))
                .child("Tax is not calculated. Check your local tax requirements before sending.")
                .child("Drafts reserve their source entries/milestones. Cancel a draft to release them.")
                .child("Marking an invoice sent assigns its number and status only; it does not send email.")
                .child(actions)
        } else {
            div().child("Select an invoice to review it, or create a draft from unbilled work.")
        };

        let project_selection = if self.projects.is_empty() {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child("Create a project before preparing an invoice.")
                .child(self.action_button(Page::Projects, "Go to projects", cx))
        } else {
            project_rows
        };
        let invoice_list = if self.invoices.is_empty() {
            div().child("No invoices yet. Drafts you create will appear here.")
        } else {
            invoice_rows
        };

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Create a new draft")
                    .child(project_selection)
                    .child(create_panel),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Invoice history")
                    .child(invoice_list),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Invoice details")
                    .child(detail),
            )
    }

    fn settings_content(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(0xe3eee9))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(if self.business_name.trim().is_empty() {
                        "Let’s set up your workspace"
                    } else {
                        "Business and invoice defaults"
                    })
                    .child("These details appear on new invoices. You can update them at any time."),
            )
            .child(
                div()
                    .p_4()
                    .rounded_lg()
                    .bg(rgb(0xffffff))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.text_field("Business or trading name *", Field::BusinessName, cx))
                    .child(self.text_field("Business address", Field::BusinessAddress, cx))
                    .child(self.text_field("Business email or phone", Field::BusinessContact, cx))
                    .child(self.text_field("Currency code (default is USD)", Field::Currency, cx))
                    .child(self.text_field("Invoice number prefix", Field::InvoicePrefix, cx))
                    .child("Currency applies to new projects and invoices. Existing invoices keep their saved currency snapshot.")
                    .child(
                        div()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(0x254b42))
                            .text_color(rgb(0xffffff))
                            .child("Save settings")
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                                this.save_settings();
                                cx.notify();
                            })),
                    ),
            )
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = div().flex().flex_col().gap_4();
        let content = match self.page {
            Page::Overview => content.child(self.overview_content(cx)),
            Page::Time => content.child(self.time_content(cx)),
            Page::Clients | Page::Projects => content.child(self.clients_projects_content(cx)),
            Page::Invoices => content.child(self.invoices_content(cx)),
            Page::Settings => content.child(self.settings_content(cx)),
        };

        let navigation = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.navigation_button(Page::Overview, "⌂", "Overview", cx))
            .child(self.navigation_button(Page::Time, "◷", "Time tracking", cx))
            .child(self.navigation_button(Page::Clients, "●", "Clients", cx))
            .child(self.navigation_button(Page::Projects, "▦", "Projects", cx))
            .child(self.navigation_button(Page::Invoices, "▤", "Invoices", cx));

        let settings_nav = self.navigation_button(Page::Settings, "⚙", "Settings", cx);
        let database_label = if self.store.is_some() {
            "Local database connected"
        } else {
            "Database unavailable"
        };
        let sidebar = div()
            .w(px(232.))
            .h_full()
            .p_4()
            .bg(rgb(0x102622))
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .child(
                        div()
                            .text_size(px(22.))
                            .text_color(rgb(0xffffff))
                            .child("POCKETS"),
                    )
                    .child(div().text_color(rgb(0xaac0b9)).child("Freelance workspace")),
            )
            .child(div().text_color(rgb(0x8fa9a1)).child("WORKSPACE"))
            .child(navigation)
            .child(div().flex_grow())
            .child(settings_nav)
            .child(div().p_2().text_color(rgb(0x8fa9a1)).child(database_label));

        let (title, description) = self.page_heading();
        let timer_status = if self.running_timer.is_some() {
            div()
                .p_2()
                .rounded_md()
                .bg(rgb(0xdce9e5))
                .text_color(rgb(0x24463e))
                .child("Timer running")
        } else {
            div()
                .p_2()
                .rounded_md()
                .bg(rgb(0xffffff))
                .text_color(rgb(0x5b6f69))
                .child("Ready")
        };
        let page_header = div()
            .flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(26.))
                            .text_color(rgb(0x183630))
                            .child(title),
                    )
                    .child(div().text_color(rgb(0x5b6f69)).child(description)),
            )
            .child(timer_status);

        let mut messages = div().flex().flex_col().gap_2();
        if let Some(error) = &self.error {
            messages = messages.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0xfce8e6))
                    .text_color(rgb(0x8f2d26))
                    .child(format!("Needs attention: {error}")),
            );
        }
        if let Some(status) = &self.status {
            messages = messages.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0xe3eee9))
                    .text_color(rgb(0x24463e))
                    .child(status.clone()),
            );
        }

        let page_area = div()
            .id("page-content")
            .flex_grow()
            .overflow_y_scroll()
            .child(div().flex().flex_col().gap_4().child(content));
        let main = div()
            .h_full()
            .flex_grow()
            .p_6()
            .flex()
            .flex_col()
            .gap_5()
            .child(page_header)
            .child(messages)
            .child(page_area);

        let root = div()
            .size_full()
            .bg(rgb(0xf4f6f4))
            .flex()
            .on_key_down(cx.listener(|this, event, _window, cx| {
                this.handle_key(event, cx);
            }))
            .child(sidebar)
            .child(main);
        let root = match &self.focus_handle {
            Some(handle) => root.track_focus(handle),
            None => root,
        };
        root
    }
}

fn app_data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let directory = {
        let base = env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE")
                    .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
            })
            .ok_or_else(|| "APPDATA or USERPROFILE is not set".to_string())?;
        base.join("Pockets")
    };

    #[cfg(target_os = "macos")]
    let directory = {
        let home = env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Pockets")
    };

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let directory = {
        let base = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .ok_or_else(|| "XDG_DATA_HOME or HOME is not set".to_string())?;
        base.join("pockets")
    };

    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    Ok(directory)
}

fn parse_money_cents(input: &str) -> Option<i64> {
    let input = input.trim();
    if input.is_empty() || input.starts_with('-') {
        return None;
    }
    let mut parts = input.split('.');
    let whole_text = parts.next()?;
    let fraction_text = parts.next().unwrap_or("");
    if parts.next().is_some()
        || whole_text.is_empty()
        || !whole_text.bytes().all(|byte| byte.is_ascii_digit())
        || fraction_text.len() > 2
        || !fraction_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole = whole_text.parse::<i64>().ok()?;
    let fraction = match fraction_text.len() {
        0 => 0,
        1 => fraction_text.parse::<i64>().ok()? * 10,
        _ => fraction_text.parse::<i64>().ok()?,
    };
    whole.checked_mul(100)?.checked_add(fraction)
}

fn format_amount_input(cents: i64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

fn format_money(cents: i64, currency: &str) -> String {
    format!("{} {}", currency, format_amount_input(cents))
}

fn parse_time_of_day(time: &str) -> Result<(i64, i64), String> {
    let mut raw = time.trim();
    if raw.to_ascii_uppercase().ends_with("UTC") {
        raw = raw[..raw.len() - 3].trim_end();
    }

    let (time_part, period) = if let Some(stripped) = raw.strip_suffix(|c| c == 'm' || c == 'M') {
        let stripped = stripped.trim_end();
        if let Some(prefix) = stripped.strip_suffix(|c| c == 'a' || c == 'A') {
            (prefix.trim_end(), Some("AM"))
        } else if let Some(prefix) = stripped.strip_suffix(|c| c == 'p' || c == 'P') {
            (prefix.trim_end(), Some("PM"))
        } else {
            return Err("Time must use HH:MM AM/PM in UTC.".to_string());
        }
    } else {
        (raw, None)
    };

    let mut parts = time_part.split(':');
    let hour = parts
        .next()
        .and_then(|p| p.trim().parse::<i64>().ok())
        .ok_or_else(|| "Time must use HH:MM AM/PM in UTC.".to_string())?;
    let minute = parts
        .next()
        .and_then(|p| p.trim().parse::<i64>().ok())
        .ok_or_else(|| "Time must use HH:MM AM/PM in UTC.".to_string())?;

    if parts.next().is_some() || !(0..=59).contains(&minute) {
        return Err("Time must be a valid HH:MM AM/PM value in UTC.".to_string());
    }

    let hour24 = match period {
        Some("AM") => {
            if !(1..=12).contains(&hour) {
                return Err("Hour must be between 1 and 12 in 12-hour format.".to_string());
            }
            if hour == 12 {
                0
            } else {
                hour
            }
        }
        Some("PM") => {
            if !(1..=12).contains(&hour) {
                return Err("Hour must be between 1 and 12 in 12-hour format.".to_string());
            }
            if hour == 12 {
                12
            } else {
                hour + 12
            }
        }
        None => {
            if !(0..=23).contains(&hour) {
                return Err("Time must be a valid HH:MM AM/PM value in UTC.".to_string());
            }
            hour
        }
        _ => return Err("Time must use HH:MM AM/PM in UTC.".to_string()),
    };

    Ok((hour24, minute))
}

fn parse_utc_datetime(date: &str, time: &str) -> Result<i64, String> {
    let midnight = domain::iso_date_to_unix_seconds(date).map_err(|error| error.to_string())?;
    let (hour, minute) = parse_time_of_day(time)?;
    midnight
        .checked_add(hour * 3_600 + minute * 60)
        .ok_or_else(|| "Date and time are outside the supported range.".to_string())
}

fn format_time_utc(timestamp: i64) -> String {
    let seconds = timestamp.rem_euclid(86_400);
    let hour24 = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let (hour12, period) = match hour24 {
        0 => (12, "AM"),
        1..=11 => (hour24, "AM"),
        12 => (12, "PM"),
        _ => (hour24 - 12, "PM"),
    };
    format!("{:02}:{:02} {}", hour12, minute, period)
}

fn format_timestamp_utc(timestamp: i64) -> String {
    format!(
        "{} {}",
        domain::iso_date_from_unix(timestamp),
        format_time_utc(timestamp)
    )
}

fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

fn display_description(description: &str) -> &str {
    if description.trim().is_empty() {
        "(no description)"
    } else {
        description
    }
}

fn safe_filename(value: &str) -> String {
    let filename: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    if filename.is_empty() {
        "invoice".to_string()
    } else {
        filename
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn invoice_html(document: &storage::InvoiceDocument) -> String {
    let invoice = &document.invoice;
    let number = invoice
        .number
        .clone()
        .unwrap_or_else(|| format!("Draft {}", invoice.id));
    let mut html = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Invoice</title><style>body{font:16px system-ui,sans-serif;max-width:900px;margin:48px auto;padding:0 24px;color:#1c1917}header,.meta{display:flex;justify-content:space-between;gap:32px}h1{font-size:34px}table{width:100%;border-collapse:collapse;margin:32px 0}th,td{text-align:left;padding:12px;border-bottom:1px solid #d6d3d1}td:last-child,th:last-child{text-align:right}.total{text-align:right;font-size:22px;font-weight:700}.muted{color:#57534e}.print{margin:24px 0;padding:10px 16px}@media print{.print{display:none}body{margin:0;max-width:none}}</style></head><body>",
    );
    html.push_str(&format!(
        "<button class=\"print\" onclick=\"window.print()\">Print / Save as PDF</button><header><section><h1>Invoice</h1><div><strong>{}</strong></div><div>{}</div><div>{}</div></section><section><div><strong>{}</strong></div><div>Status: {:?}</div><div>Issued: {}</div><div>Due: {}</div></section></header><hr><h2>Bill to</h2><div><strong>{}</strong></div><div>{}</div><div>{}</div><p>Project: {}</p><table><thead><tr><th>Description</th><th>Time</th><th>Amount</th></tr></thead><tbody>",
        html_escape(&document.business_name),
        html_escape(&document.business_address),
        html_escape(&document.business_contact),
        html_escape(&number),
        invoice.status,
        html_escape(&invoice.issue_date),
        html_escape(&invoice.due_date),
        html_escape(&document.client.name),
        html_escape(&document.client.address),
        html_escape(&document.client.contact),
        html_escape(&document.project.name),
    ));
    for line in &document.lines {
        let quantity = line
            .quantity_seconds
            .map(format_duration)
            .unwrap_or_else(|| "—".to_string());
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            html_escape(&line.description),
            html_escape(&quantity),
            html_escape(&format_money(line.amount_cents, &document.currency))
        ));
    }
    html.push_str(&format!(
        "</tbody></table><p class=\"total\">Total: {}</p><p class=\"muted\">Tax is not calculated. Check applicable tax requirements before sending.</p><p class=\"muted\">Invoice {} · generated with Pockets</p></body></html>",
        html_escape(&format_money(invoice.total_cents, &document.currency)),
        html_escape(&number)
    ));
    html
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let (store, data_dir, db_status) = match app_data_dir() {
            Ok(data_dir) => {
                let path = data_dir.join("pockets.db");
                let migration_error = if !path.exists() {
                    let legacy_path = PathBuf::from("pockets.db");
                    if legacy_path.is_file() {
                        fs::copy(&legacy_path, &path).err().map(|error| {
                            format!(
                                "could not copy legacy database from {}: {error}",
                                legacy_path.display()
                            )
                        })
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(error) = migration_error {
                    (None, data_dir, format!("Database migration error: {error}"))
                } else {
                    match storage::Store::open(&path) {
                        Ok(store) => (
                            Some(store),
                            data_dir,
                            format!("Database: {}", path.display()),
                        ),
                        Err(error) => (
                            None,
                            data_dir,
                            format!("Database error at {}: {error}", path.display()),
                        ),
                    }
                }
            }
            Err(error) => (
                None,
                PathBuf::new(),
                format!("Database unavailable: {error}"),
            ),
        };
        cx.open_window(WindowOptions::default(), move |_, cx| {
            cx.new(move |cx| Shell::new(store, data_dir, db_status, cx))
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod text_edit_tests {
    use super::*;

    #[test]
    fn cursor_movement_respects_unicode_character_boundaries() {
        let value = "café";
        assert_eq!(previous_char_boundary(value, value.len()), 3);
        assert_eq!(next_char_boundary(value, 3), value.len());
        assert_eq!(clamp_char_boundary(value, 4), 3);
    }

    #[test]
    fn insert_backspace_and_delete_operate_at_the_cursor() {
        let mut value = "café".to_string();
        let cursor = insert_text_at(&mut value, 2, "🙂");
        assert_eq!(value, "ca🙂fé");
        assert_eq!(cursor, 6);

        let (cursor, changed) = backspace_at(&mut value, cursor);
        assert!(changed);
        assert_eq!(value, "café");
        assert_eq!(cursor, 2);

        let (cursor, changed) = delete_at(&mut value, cursor);
        assert!(changed);
        assert_eq!(value, "caé");
        assert_eq!(cursor, 2);
    }
}

/// Headless tests for the app shell: the page state machine and the
/// create → track → invoice workflow, driven without a window.
#[cfg(test)]
mod shell_tests {
    use super::*;

    fn shell() -> Shell {
        Shell::build(
            Some(storage::Store::open_in_memory().expect("in-memory store")),
            PathBuf::from("."),
            "test database".to_string(),
            None,
        )
    }

    fn error_text(shell: &Shell) -> String {
        shell.error.clone().unwrap_or_default()
    }

    #[test]
    fn first_run_opens_settings_until_a_business_name_exists() {
        assert_eq!(shell().page, Page::Settings);
    }

    #[test]
    fn saving_settings_persists_values_and_advances_to_the_overview() {
        let mut app = shell();
        app.business_name = "Acme Studio".to_string();
        app.currency = "eur".to_string();
        app.invoice_prefix = "acme-".to_string();
        app.save_settings();

        assert!(
            app.error.is_none(),
            "unexpected error: {}",
            error_text(&app)
        );
        assert_eq!(app.page, Page::Overview);
        assert_eq!(app.currency, "EUR");
        assert_eq!(app.invoice_prefix, "ACME-");

        let store = app.store.as_ref().expect("store");
        assert_eq!(
            store.get_setting("business_name").unwrap().as_deref(),
            Some("Acme Studio")
        );
    }

    #[test]
    fn an_invalid_currency_is_rejected_before_anything_is_written() {
        let mut app = shell();
        app.business_name = "Acme".to_string();
        app.currency = "EURO".to_string();
        app.save_settings();

        assert!(error_text(&app).contains("Currency"));
        assert_eq!(app.page, Page::Settings);
        let store = app.store.as_ref().expect("store");
        // The seeded default survives; "Acme" was never written.
        assert_eq!(
            store.get_setting("business_name").unwrap().as_deref(),
            Some("")
        );
    }

    #[test]
    fn an_invalid_invoice_prefix_is_rejected() {
        let mut app = shell();
        app.business_name = "Acme".to_string();
        app.invoice_prefix = "no spaces".to_string();
        app.save_settings();

        assert!(error_text(&app).contains("prefix"));
        assert_eq!(app.page, Page::Settings);
    }

    #[test]
    fn creating_a_client_persists_and_selects_it() {
        let mut app = shell();
        app.client_name = "Northwind".to_string();
        app.client_contact = "ops@northwind.test".to_string();
        app.payment_terms = "14".to_string();
        app.save_client();

        assert!(
            app.error.is_none(),
            "unexpected error: {}",
            error_text(&app)
        );
        assert_eq!(app.clients.len(), 1);
        assert_eq!(app.clients[0].name, "Northwind");
        assert_eq!(app.clients[0].payment_terms_days, 14);
        assert_eq!(app.selected_client_id, Some(app.clients[0].id));
        // The form resets so the next entry starts clean.
        assert_eq!(app.client_name, "");
        assert_eq!(app.payment_terms, "30");
    }

    #[test]
    fn a_client_without_a_name_is_rejected() {
        let mut app = shell();
        app.client_name = "   ".to_string();
        app.save_client();

        assert_eq!(app.clients.len(), 0);
        assert!(error_text(&app).contains("Client name"));
    }

    #[test]
    fn payment_terms_outside_the_allowed_range_are_rejected() {
        let mut app = shell();
        app.client_name = "Northwind".to_string();
        app.payment_terms = "99999".to_string();
        app.save_client();

        assert_eq!(app.clients.len(), 0);
        assert!(error_text(&app).contains("Payment terms"));
    }

    #[test]
    fn starting_a_timer_without_a_project_reports_an_error() {
        let mut app = shell();
        app.start_timer();

        assert!(app.running_timer.is_none());
        assert!(error_text(&app).contains("project"));
    }

    #[test]
    fn creating_a_project_without_a_client_reports_an_error() {
        let mut app = shell();
        app.project_name = "Website".to_string();
        app.create_project();

        assert_eq!(app.projects.len(), 0);
        assert!(error_text(&app).contains("client"));
    }

    #[test]
    fn creating_an_invoice_without_a_business_name_routes_to_settings() {
        let mut app = shell();
        app.business_name = "Acme".to_string();
        app.save_settings();
        assert_eq!(app.page, Page::Overview);

        app.business_name.clear();
        app.create_invoice();

        assert_eq!(app.page, Page::Settings);
        assert!(error_text(&app).contains("business name"));
        assert_eq!(app.invoices.len(), 0);
    }

    /// The headline workflow: set up a business, add a client and project,
    /// record billable time, draft an invoice, and send it.
    #[test]
    fn create_track_and_invoice_flow_runs_end_to_end() {
        let mut app = shell();

        app.business_name = "Acme Studio".to_string();
        app.save_settings();
        assert_eq!(app.page, Page::Overview);

        app.client_name = "Northwind".to_string();
        app.save_client();
        assert!(
            app.error.is_none(),
            "client step failed: {}",
            error_text(&app)
        );

        app.project_name = "Website".to_string();
        app.project_rate = "120.00".to_string();
        app.create_project();
        assert!(
            app.error.is_none(),
            "project step failed: {}",
            error_text(&app)
        );
        let project_id = app.selected_project_id.expect("project selected");

        // One deterministic billable hour, independent of the wall clock.
        app.manual_description = "Design".to_string();
        app.manual_date = "2024-03-04".to_string();
        app.manual_start = "09:00 AM".to_string();
        app.manual_end_date = "2024-03-04".to_string();
        app.manual_end = "10:00 AM".to_string();
        app.manual_billable = true;
        app.save_manual_entry();
        assert!(
            app.error.is_none(),
            "time step failed: {}",
            error_text(&app)
        );
        assert_eq!(app.entries.len(), 1);

        app.create_invoice();
        assert!(
            app.error.is_none(),
            "invoice step failed: {}",
            error_text(&app)
        );
        let invoice = app.invoices[0].clone();
        assert_eq!(invoice.status, domain::InvoiceStatus::Draft);
        assert_eq!(invoice.project_id, project_id);
        // One hour at 120.00 is 12,000 cents.
        assert_eq!(invoice.total_cents, 12_000);

        app.selected_invoice_id = Some(invoice.id);
        app.mark_invoice_sent();
        assert!(
            app.error.is_none(),
            "send step failed: {}",
            error_text(&app)
        );
        let sent = app
            .invoices
            .iter()
            .find(|item| item.id == invoice.id)
            .expect("invoice");
        assert_eq!(sent.status, domain::InvoiceStatus::Sent);
        assert_eq!(sent.number.as_deref(), Some("INV-0001"));

        // Sending reserves the billable time, so it cannot be invoiced twice.
        app.create_invoice();
        assert!(error_text(&app).contains("nothing"), "{}", error_text(&app));
        assert_eq!(app.invoices.len(), 1);
    }

    #[test]
    fn refresh_keeps_a_valid_selection_and_replaces_a_deleted_one() {
        let mut app = shell();
        app.client_name = "Northwind".to_string();
        app.save_client();
        app.client_name = "Contoso".to_string();
        app.save_client();
        assert_eq!(app.clients.len(), 2);

        let first = app.clients[0].id;
        let second = app.clients[1].id;
        app.selected_client_id = Some(first);
        app.refresh();
        assert_eq!(app.selected_client_id, Some(first));

        app.selected_client_id = Some(9_999);
        app.refresh();
        assert_eq!(app.selected_client_id, Some(first));
        assert_eq!(app.selected_client_id, app.clients.first().map(|c| c.id));
        assert!(app.clients.iter().any(|c| c.id == second));
    }

    #[test]
    fn format_time_utc_uses_12_hour_am_pm() {
        assert_eq!(format_time_utc(0), "12:00 AM");
        assert_eq!(format_time_utc(1_800), "12:30 AM");
        assert_eq!(format_time_utc(3_600), "01:00 AM");
        assert_eq!(format_time_utc(9 * 3_600), "09:00 AM");
        assert_eq!(format_time_utc(11 * 3_600 + 59 * 60), "11:59 AM");
        assert_eq!(format_time_utc(12 * 3_600), "12:00 PM");
        assert_eq!(format_time_utc(12 * 3_600 + 30 * 60), "12:30 PM");
        assert_eq!(format_time_utc(13 * 3_600), "01:00 PM");
        assert_eq!(format_time_utc(23 * 3_600 + 59 * 60), "11:59 PM");
    }

    #[test]
    fn format_timestamp_utc_includes_date_and_12_hour_time() {
        let ts = domain::iso_date_to_unix_seconds("2024-03-04").unwrap() + 9 * 3_600;
        assert_eq!(format_timestamp_utc(ts), "2024-03-04 09:00 AM");

        let ts_pm = domain::iso_date_to_unix_seconds("2024-03-04").unwrap() + 15 * 3_600 + 30 * 60;
        assert_eq!(format_timestamp_utc(ts_pm), "2024-03-04 03:30 PM");
    }

    #[test]
    fn parse_utc_datetime_accepts_12_hour_am_pm_variants() {
        let base = domain::iso_date_to_unix_seconds("2024-03-04").unwrap();

        assert_eq!(parse_utc_datetime("2024-03-04", "12:00 AM").unwrap(), base);
        assert_eq!(parse_utc_datetime("2024-03-04", "12:00 am").unwrap(), base);
        assert_eq!(parse_utc_datetime("2024-03-04", "12:00am").unwrap(), base);
        assert_eq!(parse_utc_datetime("2024-03-04", "09:00 AM").unwrap(), base + 9 * 3_600);
        assert_eq!(parse_utc_datetime("2024-03-04", "9:00 AM").unwrap(), base + 9 * 3_600);
        assert_eq!(parse_utc_datetime("2024-03-04", "9:00 AM UTC").unwrap(), base + 9 * 3_600);
        assert_eq!(parse_utc_datetime("2024-03-04", "12:00 PM").unwrap(), base + 12 * 3_600);
        assert_eq!(
            parse_utc_datetime("2024-03-04", "01:30 PM").unwrap(),
            base + 13 * 3_600 + 30 * 60
        );
        assert_eq!(
            parse_utc_datetime("2024-03-04", "1:30 pm").unwrap(),
            base + 13 * 3_600 + 30 * 60
        );
        assert_eq!(
            parse_utc_datetime("2024-03-04", "11:59 PM").unwrap(),
            base + 23 * 3_600 + 59 * 60
        );
    }

    #[test]
    fn parse_utc_datetime_accepts_24_hour_fallback() {
        let base = domain::iso_date_to_unix_seconds("2024-03-04").unwrap();
        assert_eq!(parse_utc_datetime("2024-03-04", "09:00").unwrap(), base + 9 * 3_600);
        assert_eq!(
            parse_utc_datetime("2024-03-04", "13:30").unwrap(),
            base + 13 * 3_600 + 30 * 60
        );
    }

    #[test]
    fn parse_utc_datetime_rejects_invalid_times() {
        assert!(parse_utc_datetime("2024-03-04", "00:00 AM").is_err());
        assert!(parse_utc_datetime("2024-03-04", "13:00 AM").is_err());
        assert!(parse_utc_datetime("2024-03-04", "00:00 PM").is_err());
        assert!(parse_utc_datetime("2024-03-04", "13:00 PM").is_err());
        assert!(parse_utc_datetime("2024-03-04", "12:60 AM").is_err());
        assert!(parse_utc_datetime("2024-03-04", "invalid").is_err());
    }

    #[test]
    fn editing_entry_populates_12_hour_format() {
        let mut app = shell();
        app.business_name = "Acme".to_string();
        app.save_settings();
        app.client_name = "Northwind".to_string();
        app.save_client();
        app.project_name = "Website".to_string();
        app.create_project();

        app.manual_description = "Design".to_string();
        app.manual_date = "2024-03-04".to_string();
        app.manual_start = "02:30 PM".to_string();
        app.manual_end_date = "2024-03-04".to_string();
        app.manual_end = "03:45 PM".to_string();
        app.save_manual_entry();

        let entry_id = app.entries[0].id;
        app.begin_edit_entry(entry_id);

        assert_eq!(app.manual_start, "02:30 PM");
        assert_eq!(app.manual_end, "03:45 PM");
    }
}

/// Drives the real UI: renders `Shell` into a headless GPUI window and
/// dispatches genuine mouse events through the platform event path, so
/// hit-testing, listeners and focus all run exactly as they do in the app.
#[cfg(test)]
mod ui_click_tests {
    use super::*;
    use gpui::{
        Bounds, Entity, Modifiers, Pixels, ScrollDelta, ScrollWheelEvent, TestAppContext,
        TouchPhase, VisualTestContext, point, size,
    };

    const WINDOW_W: f32 = 1000.0;
    const WINDOW_H: f32 = 760.0;
    // The nav rail is 232px wide with 16px padding on every side.
    const NAV_CLICK_X: f32 = 116.0;
    // Nav items are 48px tall (p_3 padding around 24px of content) and are
    // stacked with an 8px gap, so they repeat on a 56px pitch. The first item
    // starts below the 16px rail padding, the brand block and the
    // "WORKSPACE" label, which puts its centre at ~196px.
    const NAV_FIRST_CENTER_Y: f32 = 196.0;
    const NAV_ITEM_PITCH: f32 = 56.0;
    // The Settings item is pushed to the bottom of the rail by a flex spacer.
    const NAV_SETTINGS_CENTER_Y: f32 = 632.0;
    // The content column starts at x = 232 (rail) + 24 (p_6) = 256, so x=400
    // lands inside a full-width form input. These are input centres that were
    // verified by clicking them, not a derived pitch.
    const FORM_CLICK_X: f32 = 400.0;
    const FIELD_BUSINESS_NAME_Y: f32 = 320.0;
    const FIELD_BUSINESS_ADDRESS_Y: f32 = 420.0;
    const FIELD_CURRENCY_Y: f32 = 600.0;
    const FIELD_INVOICE_PREFIX_Y: f32 = 700.0;

    // "Save settings" sits at y≈798, below the fold of the 760px test window.
    // The content column is top-anchored, so the button stays at that offset in
    // any window tall enough to show it.
    const SETTINGS_SAVE_Y: f32 = 798.0;
    // Height the settings page needs before "Save settings" is on screen at
    // all; below this the form has to be scrolled.
    const SETTINGS_FORM_BOTTOM: f32 = 820.0;
    // Once the content is scrolled to its end, the save button comes to rest
    // around here in the 760px window.
    const SCROLLED_SAVE_Y: f32 = 680.0;

    fn open_shell(cx: &mut TestAppContext) -> (Entity<Shell>, VisualTestContext) {
        open_shell_sized(cx, WINDOW_W, WINDOW_H)
    }

    /// Opens the shell the way `main` does, with `WindowOptions::default()`.
    fn open_shell_default(cx: &mut TestAppContext) -> (Entity<Shell>, VisualTestContext) {
        open_shell_with_bounds(cx, None)
    }

    fn open_shell_sized(
        cx: &mut TestAppContext,
        window_w: f32,
        window_h: f32,
    ) -> (Entity<Shell>, VisualTestContext) {
        open_shell_with_bounds(
            cx,
            Some(Bounds::new(
                point(px(0.), px(0.)),
                size(px(window_w), px(window_h)),
            )),
        )
    }

    /// Opens the shell the way `main` does. Passing `None` keeps
    /// `WindowOptions::default()`, which is how the real app opens its window -
    /// that is the size the app is actually launched at.
    fn open_shell_with_bounds(
        cx: &mut TestAppContext,
        bounds: Option<Bounds<Pixels>>,
    ) -> (Entity<Shell>, VisualTestContext) {
        let mut shell = None;
        let handle = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: bounds.map(gpui::WindowBounds::Windowed),
                    ..Default::default()
                },
                |_window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(
                            Some(storage::Store::open_in_memory().expect("store")),
                            PathBuf::from("."),
                            "test database".to_string(),
                            cx,
                        )
                    });
                    shell = Some(entity.clone());
                    entity
                },
            )
            .expect("open window")
        });
        let visual = VisualTestContext::from_window(handle.into(), cx);
        (shell.expect("shell entity"), visual)
    }

    fn click(visual: &mut VisualTestContext, x: f32, y: f32) {
        visual.simulate_click(point(px(x), px(y)), Modifiers::none());
        visual.run_until_parked();
    }

    fn state(visual: &mut VisualTestContext, shell: &Entity<Shell>) -> (Page, Option<Field>) {
        visual.update(|_, cx| {
            let shell = shell.read(cx);
            (shell.page, shell.active_field)
        })
    }

    // Wheel scrolling inside the content area. The settings form is taller than
    // a short window, so the save button only becomes reachable once scrolled.
    // Scrolling far past the end clamps, which makes the result deterministic.
    fn scroll_content(visual: &mut VisualTestContext, events: usize) {
        for _ in 0..events {
            visual.simulate_event(ScrollWheelEvent {
                position: point(px(FORM_CLICK_X), px(400.0)),
                delta: ScrollDelta::Lines(point(0.0, -1.0)),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
        }
        visual.run_until_parked();
    }

    #[test]
    fn saving_settings_works_in_a_window_too_short_for_the_form() {
        let mut cx = TestAppContext::single();
        let (shell, mut visual) = open_shell(&mut cx);
        assert_eq!(state(&mut visual, &shell).0, Page::Settings);

        // At 760px the save button sits below the fold: without scrolling, a
        // click near the bottom edge must not reach it.
        click(&mut visual, FORM_CLICK_X, WINDOW_H - 40.0);
        assert_eq!(
            state(&mut visual, &shell).0,
            Page::Settings,
            "the save button is below the fold before scrolling"
        );

        click(&mut visual, FORM_CLICK_X, FIELD_BUSINESS_NAME_Y);
        visual.simulate_input("Acme Studio");
        visual.run_until_parked();

        // Scrolling far past the end must clamp, leaving the button pinned near
        // the bottom of the visible area, where it can finally be clicked.
        scroll_content(&mut visual, 400);
        click(&mut visual, FORM_CLICK_X, SCROLLED_SAVE_Y);
        visual.run_until_parked();

        let (page, error, status) = visual.update(|_, cx| {
            let shell = shell.read(cx);
            (shell.page, shell.error.clone(), shell.status.clone())
        });
        assert_eq!(error, None, "save reported an error: {error:?}");
        assert_eq!(page, Page::Overview, "a scrolled save must go through");
        assert!(
            status
                .as_deref()
                .is_some_and(|status| status.contains("settings saved")),
            "expected a confirmation, got {status:?}"
        );
    }

    #[test]
    fn the_default_window_is_tall_enough_to_show_the_save_button() {
        // The settings test clicks the button without scrolling, so the app's
        // real default window has to be tall enough to put it on screen. This
        // pins that assumption instead of hardcoding a window size.
        let mut cx = TestAppContext::single();
        let (_, mut visual) = open_shell_default(&mut cx);

        let height = visual.update(|window, _| f32::from(window.viewport_size().height));
        assert!(
            height >= SETTINGS_FORM_BOTTOM,
            "default viewport is {height}px, so the save button would need scrolling"
        );
    }

    #[test]
    fn filling_the_settings_form_and_clicking_save_persists_and_advances() {
        let mut cx = TestAppContext::single();
        let (shell, mut visual) = open_shell_default(&mut cx);
        assert_eq!(state(&mut visual, &shell).0, Page::Settings);

        click(&mut visual, FORM_CLICK_X, FIELD_BUSINESS_NAME_Y);
        visual.simulate_input("  Acme Studio  ");
        visual.run_until_parked();
        assert_eq!(
            state(&mut visual, &shell).1,
            Some(Field::BusinessName),
            "typing must not move focus"
        );

        click(&mut visual, FORM_CLICK_X, SETTINGS_SAVE_Y);

        let (page, field, error, status) = visual.update(|_, cx| {
            let shell = shell.read(cx);
            (
                shell.page,
                shell.active_field,
                shell.error.clone(),
                shell.status.clone(),
            )
        });
        assert_eq!(error, None, "save should not report an error");
        assert_eq!(page, Page::Overview, "saving closes the settings page");
        assert_eq!(field, None, "saving releases focus");
        assert!(
            status
                .as_deref()
                .is_some_and(|status| status.contains("settings saved")),
            "expected a confirmation, got {status:?}"
        );

        // The click has to reach the database, not just the in-memory field,
        // and the surrounding whitespace has to be trimmed on the way in.
        let stored = visual.update(|_, cx| {
            let shell = shell.read(cx);
            shell
                .store
                .as_ref()
                .expect("store")
                .get_setting("business_name")
                .expect("read setting")
        });
        assert_eq!(stored.as_deref(), Some("Acme Studio"));
    }

    #[test]
    fn every_settings_field_can_be_clicked_to_take_focus() {
        let mut cx = TestAppContext::single();
        let (shell, mut visual) = open_shell(&mut cx);

        for (field, y) in [
            (Field::BusinessName, FIELD_BUSINESS_NAME_Y),
            (Field::BusinessAddress, FIELD_BUSINESS_ADDRESS_Y),
            (Field::Currency, FIELD_CURRENCY_Y),
            (Field::InvoicePrefix, FIELD_INVOICE_PREFIX_Y),
        ] {
            click(&mut visual, FORM_CLICK_X, y);
            assert_eq!(
                state(&mut visual, &shell).1,
                Some(field),
                "clicking at y={y} should focus {field:?}"
            );
        }
    }

    #[test]
    fn the_nav_rail_switches_pages_when_clicked() {
        let mut cx = TestAppContext::single();
        let (shell, mut visual) = open_shell(&mut cx);
        assert_eq!(
            state(&mut visual, &shell).0,
            Page::Settings,
            "first run opens settings"
        );

        for (index, page) in [
            Page::Overview,
            Page::Time,
            Page::Clients,
            Page::Projects,
            Page::Invoices,
        ]
        .into_iter()
        .enumerate()
        {
            let y = NAV_FIRST_CENTER_Y + index as f32 * NAV_ITEM_PITCH;
            click(&mut visual, NAV_CLICK_X, y);
            assert_eq!(
                state(&mut visual, &shell).0,
                page,
                "clicking nav item {index} at y={y}"
            );
        }

        click(&mut visual, NAV_CLICK_X, NAV_SETTINGS_CENTER_Y);
        assert_eq!(state(&mut visual, &shell).0, Page::Settings);
    }

    #[test]
    fn clicking_a_field_focuses_only_that_field_and_typing_edits_it() {
        let mut cx = TestAppContext::single();
        let (shell, mut visual) = open_shell(&mut cx);

        click(&mut visual, FORM_CLICK_X, FIELD_BUSINESS_NAME_Y);
        assert_eq!(state(&mut visual, &shell).1, Some(Field::BusinessName));

        visual.simulate_input("Acme Studio");
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| shell.read(cx).business_name.clone()),
            "Acme Studio"
        );

        click(&mut visual, FORM_CLICK_X, FIELD_BUSINESS_ADDRESS_Y);
        assert_eq!(state(&mut visual, &shell).1, Some(Field::BusinessAddress));

        visual.simulate_input("12 Main St");
        visual.run_until_parked();
        assert_eq!(
            visual.update(|_, cx| shell.read(cx).business_address.clone()),
            "12 Main St"
        );
        // Typing in the second field must not disturb the first.
        assert_eq!(
            visual.update(|_, cx| shell.read(cx).business_name.clone()),
            "Acme Studio"
        );
    }

    #[test]
    fn pressing_enter_releases_the_focused_field() {
        let mut cx = TestAppContext::single();
        let (shell, mut visual) = open_shell(&mut cx);

        click(&mut visual, FORM_CLICK_X, FIELD_BUSINESS_NAME_Y);
        assert_eq!(state(&mut visual, &shell).1, Some(Field::BusinessName));

        visual.simulate_keystrokes("enter");
        visual.run_until_parked();
        assert_eq!(state(&mut visual, &shell).1, None);
    }
}
