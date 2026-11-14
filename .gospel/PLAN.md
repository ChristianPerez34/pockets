# Plan

## Goal
Replace Pockets' flat, button-heavy presentation with a clear, navigatable desktop UI while preserving the existing domain and storage workflows.

## Steps
- [x] Audit the GPUI shell, page state, handlers, and persistence.
- [x] Restructure the shell into a persistent left navigation rail and distinct page content with active-route styling.
- [x] Add separate Clients and Projects routes using the existing CRUD and milestone workflows.
- [x] Add dashboard summaries/quick navigation, page headers, grouped content panels, clearer field styling, and a vertically scrollable content area.
- [x] Run formatting, compilation, build, tests, and whitespace checks.
- [ ] Visually and interactively smoke-test route changes, scrolling, first-run setup, and the create→track→invoice workflow in a desktop session.

## Evidence / Verification
- `cargo fmt --all -- --check`, `cargo check --locked --offline`, `cargo build --locked --offline`, and `git diff --check` pass.
- `cargo test --locked --offline`: 49 passed, 0 failed.
- GPUI 0.2.2 scrolling is applied to the stateful page-content element via `.id("page-content").overflow_y_scroll()`.
- Navigation now provides Overview, Time tracking, Clients, Projects, Invoices, and Settings. Sidebar selections update the active page; Dashboard quick actions also navigate. Client and project forms are on distinct routes, and time/invoice screens are grouped into panels with empty states and cross-navigation to Projects.
- This workspace session has no desktop screen/input automation, so the new layout and interactions are code/build-verified but not visually observed in a real window.
- Existing warnings remain: several unused domain/storage functions and dependency future-incompatibility notices. No test/build failures.

## Open Questions / Risks
- Need a desktop-capable interactive run to confirm actual window layout, screen transitions, scrolling, field editing, export, and restart persistence.
- Existing custom fields still do not support paste, selection, or pointer-positioned caret; mouse-only controls lack full keyboard activation.
- Distribution/installer target and invoice requirements (tax, PDF/save destination, email) remain product decisions.

## Next Action
Run the updated build in a desktop session and verify the sidebar routes and scrollable page workflows end to end.
