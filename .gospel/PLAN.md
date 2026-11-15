# Plan

## Goal
Add deterministic, headless automated tests for the GPUI app shell (navigation, forms, timer, invoicing) so app functionality is verifiable without a human clicking a window.

## Steps
- [x] Audit what is currently untested: 49 tests covered `domain` + `storage` only; zero tests for the `Shell` UI/state layer.
- [x] Confirm GPUI 0.2.2 ships `test-support` feature and `TestAppContext` (headless `TestPlatform`).
- [x] Confirm most `Shell` handlers are `&mut self` only (no `cx`), so only `Shell::new` needs a real GPUI context.
- [x] Attempt `gpui` `test-support` dev-dependency: FAILED offline — pulls `git2`/`libgit2-sys` which are absent from the vendor cache.
- [x] Fallback refactor: split `Shell::new` into `new` (supplies `Some(cx.focus_handle())`) + `build` (takes `Option<FocusHandle>`), add `focus_root` helper, make `track_focus` conditional. 5 UI call sites, no behavior change.
- [x] Add 12 headless `shell_tests` driving real handlers against an in-memory SQLite store.
- [x] Prove the tests bite: injected a deliberate regression (`payment_terms = "45"`) and confirmed it failed exactly one test, then reverted.
- [x] Full suite, fmt, build, and binary-launch smoke check all pass.

## Evidence / Verification
- `cargo test --locked --offline`: **61 passed, 0 failed** (was 49).
- `cargo fmt --all -- --check`, `cargo build --locked --offline`, `git diff --check` pass.
- Binary still launches and idles without panicking (12s run, no crash) after the `Option<FocusHandle>` change.
- Mutation check: `creating_a_client_persists_and_selects_it` failed with `left: "45", right: "30"` on the injected bug; reverted and green again.
- gpui-0.2.2 `test-support = [leak-detection, collections/test-support, rand, util/test-support, http_client/test-support, wayland, x11]`; `leak-detection` -> `backtrace` -> `git2`, unavailable offline.

## Open Questions / Risks
- Tests cover state/behavior, not rendered pixels. Layout/visual regressions still need a human or a display-automation pass.
- If the network becomes available, swapping in `gpui` `test-support` would allow true render + synthetic click tests (`TestAppContext`, `simulate_click`).
- Pre-existing dead-code warnings (`is_overdue`, `project_total_seconds`, `project_milestone_total`, `duplicate_invoice_draft`) remain in non-test builds.
- Product decisions still open: tax, PDF destination, email delivery, installer target.

## Next Action
Commit the shell test harness; then optionally add tests for project/milestone edit paths and invoice cancel/export.
