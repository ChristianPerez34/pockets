# Plan

## Goal
Add deterministic automated tests for the GPUI app, including real UI interaction (clicks, typing) driven headlessly, so app behaviour is verifiable without a human using the window.

## Steps
- [x] Add 12 headless `shell_tests` driving `&mut self` handlers against in-memory SQLite (commit 068421f).
- [x] Enable `gpui` `test-support` as a dev-dependency and drive real mouse/key events through `VisualTestContext` (commit 294dae4): nav switching, field focus + typing, Enter releasing focus.
- [x] Calibrate layout geometry empirically with throwaway probe tests (nav pitch, form field positions, Save button).
- [x] Add the first full user-task test: fill BusinessName via a click + typing, click "Save settings", assert no error, page advances to Overview, focus is released, confirmation status is set, and the value is trimmed + persisted to SQLite.
- [x] Prove it bites: dropping `.trim()` from `save_settings` failed with `left: Some("  Acme Studio  "), right: Some("Acme Studio")`.
- [x] Measure the app's real default viewport (1536×864) and use it for the settings test rather than an arbitrary size.
- [x] Full suite (65), fmt, build, whitespace check all pass.

## Evidence / Verification
- `cargo test --locked`: **65 passed, 0 failed**.
- `cargo build --locked`: bin compiles with only the two pre-existing dead-code warnings.
- `cargo fmt --all -- --check` and `git diff --check` clean; no probe/println leftovers.
- Calibrated geometry (1000×760 window unless noted): nav items 48px on a 56px pitch, first centre y≈196, Settings pinned at y≈632; rail 232px, x=116 inside it.
- Settings form (top-anchored, x=400): fields on a 96px pitch — BusinessName ≈320, BusinessAddress ≈416, BusinessContact ≈512, Currency ≈608, InvoicePrefix ≈704, each 48px tall; hint text then "Save settings" spanning y≈778–820 (centre 798).
- Real default viewport measured via `window.viewport_size()` with `WindowOptions::default()`: 1536×864.

## Open Questions / Risks
- **Save settings needs ~820px of window height and the page does not scroll.** The default 864px viewport leaves only ~44px of headroom; a user who resizes below ~820 cannot save settings at all, with no scroll affordance. Not a bug out of the box, but a real fragility. Fix would be `overflow_y_scroll` on the content column, which changes layout and requires recalibrating every coordinate above.
- Coordinate constants remain the main fragility: tied to font metrics and `p_3`/`p_6`/`gap_*` in the markup. Driving render from shared constants would remove the duplication.
- No pixel-level assertions: a visually broken but correctly-wired layout still passes. Screenshot comparison is the next tier.
- Dev-only dependency cost: `test-support` pulls backtrace -> git2/libgit2-sys, adding native build time to `cargo test` (one-time).
- Pre-existing dead-code warnings (`is_overdue`, `project_total_seconds`, `project_milestone_total`, `duplicate_invoice_draft`) remain in non-test builds.

## Next Action
Commit the settings-save test; then decide on the no-scroll clipping risk — either add `overflow_y_scroll` to the content column (and recalibrate coordinates) or shrink the settings vertical rhythm so the Save button fits a 700px window.
