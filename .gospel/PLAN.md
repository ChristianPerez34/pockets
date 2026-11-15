# Plan

## Goal
Add deterministic automated tests for the GPUI app, including real UI interaction (clicks, typing, scrolling) driven headlessly, so app behaviour is verifiable without a human using the window.

## Steps
- [x] Add 12 headless `shell_tests` driving `&mut self` handlers against in-memory SQLite (commit 068421f).
- [x] Enable `gpui` `test-support` as a dev-dependency and drive real mouse/key events through `VisualTestContext` (commit 294dae4): nav switching, field focus + typing, Enter releasing focus.
- [x] Full user-task test: fill BusinessName via click + typing, click "Save settings", assert advance to Overview, focus released, confirmation set, value trimmed + persisted.
- [x] Measure the app's real default viewport instead of hardcoding one; open the settings test with `WindowOptions::default()` exactly as `main` does.
- [x] Cover the short-window scroll path, plus pin all four settings click targets (commit 85b73c7).
- [x] Full suite (68), fmt, build all pass.

## Corrections To Earlier Notes
- **Earlier claim "the settings page does not scroll" was wrong.** `src/main.rs:2428` already builds `#page-content` with `.flex_grow()` + `.overflow_y_scroll()`, and every page renders inside it. The proposed fix would have been a no-op; no production code was changed.
- **Earlier claim "below ~820px settings cannot be saved at all" was also wrong.** Simulated `ScrollWheelEvent` scrolls the content, and a click after scrolling reaches the button and saves. That failure mode only appears if the scroll rule is removed.
- The old `~90px pitch` comment did not match the constants; replaced with field centres that are now asserted by a test.

## Evidence / Verification
- `cargo test --locked`: **68 passed, 0 failed**.
- `cargo fmt --all -- --check` and `git diff --check` clean; no probe/`println!` leftovers.
- Default viewport asserted, not assumed: `the_default_window_is_tall_enough_to_show_the_save_button` reads `window.viewport_size()` from a `WindowOptions::default()` window and fails below 820px (measures 864px).
- Mutation check on the scroll rule: deleting `.overflow_y_scroll()` fails `saving_settings_works_in_a_window_too_short_for_the_form` with `left: Settings, right: Overview`. Restored.
- Mutation check on the save path: dropping `.trim()` from `save_settings` fails with `left: Some("  Acme Studio  "), right: Some("Acme Studio")`.
- Verified click targets (1000x760): nav items 56px pitch from 196, Settings nav 632; form x=400; BusinessName 320, BusinessAddress 420, Currency 600, InvoicePrefix 700; Save spans y≈778-820 (798 at rest, ~680 once scrolled to the end).

## Open Questions / Risks
- Coordinate constants are the main fragility: tied to font metrics and `p_3`/`p_6`/`gap_*`. Driving render from shared constants would remove the duplication.
- No pixel-level assertions: a visually broken but correctly-wired layout still passes. Screenshot comparison is the next tier.
- `SCROLLED_SAVE_Y = 680` assumes wheel scrolling clamps at the content end for the 400-event scroll; a different scroll granularity in gpui could shift it.
- Pre-existing dead-code warnings (`is_overdue`, `project_total_seconds`, `project_milestone_total`, `duplicate_invoice_draft`) remain in non-test builds.

## Next Action
Extend UI coverage along the highest-value untested path: create a client and project through the form, then start and stop a timer, asserting the rows land in SQLite. The 12 headless tests already cover the domain logic underneath.
