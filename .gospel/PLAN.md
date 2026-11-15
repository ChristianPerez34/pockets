# Plan

## Goal
Add deterministic automated tests for the GPUI app, including real UI interaction (clicks, typing) driven headlessly, so app behaviour is verifiable without a human using the window.

## Steps
- [x] Add 12 headless `shell_tests` driving `&mut self` handlers against in-memory SQLite (commit 068421f).
- [x] Determine the real click-simulation path: gpui 0.2.2 `VisualTestContext::simulate_click` / `simulate_input` / `simulate_keystrokes`, all gated behind `#[cfg(any(test, feature = "test-support"))]`.
- [x] Enable it: `gpui = { version = "0.2.2", features = ["test-support"] }` as a dev-dependency. Network was reachable this time; pulls 8 crates (backtrace -> git2/libgit2-sys chain) into Cargo.lock, dev builds only.
- [x] Confirm production `cargo build --locked` is unaffected (dev-dependencies are not resolved for the bin).
- [x] Discover that `Div::debug_selector` is **private** in the published crate, so selector-based `debug_bounds` lookups are unusable from a downstream crate -> element targeting must be coordinate-based.
- [x] Empirically calibrate layout geometry with throwaway probe tests (nav rail pitch, Settings item, form field centres) instead of guessing.
- [x] Add 3 real UI tests: nav rail switches pages on click; clicking a field focuses only that field and typing edits it; Enter releases focus.
- [x] Prove they bite: forcing `this.page = Page::Overview` in the nav listener failed exactly the nav test with "clicking nav item 1 at y=252".
- [x] Full suite (64), fmt, build, whitespace check all pass.

## Evidence / Verification
- `cargo test --locked`: **64 passed, 0 failed** (49 domain/storage -> 61 with shell tests -> 64 with UI tests).
- `cargo build --locked` compiles the bin with only the two pre-existing dead-code warnings.
- `cargo fmt --all -- --check` and `git diff --check` clean; no probe/println leftovers.
- Calibrated geometry: nav items 48px tall on a 56px pitch with the first centre at y≈196 and Settings pinned at y≈632 (1000x760 window; rail 232px wide, x=116 inside it). Settings form fields repeat on a ~90px pitch: BusinessName y≈320, BusinessAddress y≈420, at x=400 (content column starts at x=256).
- Mutations confirmed the tests fail when behaviour regresses (payment terms 30->45 caught by shell test; nav target hardcoded caught by UI test).

## Open Questions / Risks
- Coordinate constants are the main fragility: they are tied to font metrics and to the `p_3`/`p_6`/`gap_*` spacing in the markup. A text-size or padding change silently shifts targets. Mitigation: the constants are named and documented, and the failure message names the item and the coordinate. Driving render from shared constants would remove the duplication.
- No pixel-level assertions: a visually broken but correctly-wired layout still passes. Screenshot comparison is the next tier.
- Visual verification still requires a human or a display-automation pass.
- Dev-only dependency cost: `leak-detection -> backtrace -> git2/libgit2-sys` adds native build time to `cargo test` (once).
- Pre-existing dead-code warnings (`is_overdue`, `project_total_seconds`, `project_milestone_total`, `duplicate_invoice_draft`) remain in non-test builds.

## Next Action
Commit the UI click tests; then extend coverage to the write path (fill the Settings form, click Save, assert persistence and the Overview transition) using the same calibrated-geometry approach.
