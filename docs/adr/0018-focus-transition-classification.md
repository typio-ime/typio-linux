# ADR-0018: Focus-transition classification and re-activation

- **Status**: Accepted
- **Date**: 2026-06-01
- **Deciders**: typio-wayland maintainers

## Context

The `zwp_input_method_v2` `done` handler decides how each compositor commit
changes focus. The original implementation drove that decision through three
predicate helpers — `should_defer_activate`, `should_cleanup_on_done`,
`should_commit_reactivation` — plus a `pending_reactivation` flag and a
`handle_reactivation()` path, intended to treat an `activate` that arrives while
already focused (clicking between fields in one window) as a lightweight
re-activation rather than a full teardown.

That machinery was broken by an incomplete refactor that shipped from the
initial commit:

- `should_defer_activate` had been changed to take a `TypioWlLifecyclePhase`
  (`return phase == DEACTIVATING`), but the only call site still passed a
  `bool` (`session_is_focused(...)`). A `bool` is `0`/`1` and never equals
  `DEACTIVATING` (`3`), so the function **always returned false**. The deferral
  branch was dead, `pending_reactivation` was never set true, and therefore
  `should_commit_reactivation` and `handle_reactivation()` were unreachable.
- `tests/test_lifecycle.c` still asserted the *original* `bool` semantics
  (`should_defer_activate(true) == true`) — but the file was **never wired into
  `meson.build`**, so the contradiction was never caught.

The runtime effect: a re-activation while focused fell through to the
no-transition branch. Nothing re-anchored the Panel or re-evaluated the
on-focus indicator for the new field, and the lifecycle phase could be left at
`ACTIVATING` (set on every `activate`) with no `done` branch resolving it back
to `ACTIVE`. This was a direct contributor to the indicator feeling unreliable
on focus changes.

## Decision

Replace the predicate-and-flag machinery with one pure classifier and a third
explicit transition.

**1. A per-batch `activate_seen` fact.** `im_handle_activate` sets
`frontend->activate_seen`; `im_handle_done` reads and clears it. This is the
signal that distinguishes a genuine (re)activation from a plain text-state
update `done` (surrounding text, content type), which the compositor also
delivers while focus is unchanged and which must **not** rebuild focus state
mid-composition.

**2. A pure `classify_done(was_active, now_active, activate_seen)`** reducing the
focus facts into one action:

| was | now | activate_seen | action |
|-----|-----|---------------|--------|
| f | t | – | `ACTIVATE` |
| t | f | – | `DEACTIVATE` |
| t | t | t | `REACTIVATE` |
| t | t | f | `NONE` (text-state update) |
| f | f | – | `NONE` |

`im_handle_done` switches on the result: `transition_to_active` /
`transition_to_inactive` / **new** `transition_to_reactivate` / no-op (keeping the
stale-grab recovery for the `NONE` case).

**3. `transition_to_reactivate`** handles "re-activated while active (a new
field)". The keyboard grab and the engine's input context persist for the same
input-method, so they are left intact; it only settles the phase back to
`ACTIVE`, re-anchors the Panel (`reset_anchor`). `ACTIVE → ACTIVATING` is added to the
valid-transition table to make this flow first-class.

**4. Pure logic is split into `engine/lifecycle_policy.c`** (the dependency-free
predicates `phase_name`, `transition_is_valid`, `phase_allows_*`,
`classify_done`), separated from `lifecycle.c`'s heavy imperative helpers that
touch the grab/vk/engine. `tests/test_lifecycle.c` is **wired into
`meson.build`** and rewritten to cover the full `classify_done` truth table and
the new transition — the unit-test safety net the focus state machine lacked.

**Deleted as dead historical baggage:** `should_defer_activate`,
`should_cleanup_on_done`, `should_commit_reactivation`, `handle_reactivation`,
the `pending_reactivation` field and its `set_pending_reactivation` setter, and
their trace fields.

## Relationship to ADR-0003

[ADR-0003](0003-session-controller-reduce-diff.md) sets the long-term direction
of a fully *derived* session controller (reduce facts → diff against observed
resources, no stored lifecycle phase). The focus path has **not** completed that
migration: it still carries an explicit `frontend->lifecycle_phase`. This ADR is
a correctness + testability improvement *within* the stored-phase model, not the
realisation of ADR-0003. `classify_done` is a small, honest "reduce focus facts
to an action" step that is compatible with — and a stepping stone toward — the
ADR-0003 model, should the stored phase later be removed. The explanation docs
that describe the reduce+diff model as already complete (`timing-model.md`,
`wayland-input-method.md`) overstate the current code; reconciling them with the
phase-machine reality is tracked separately.

## Alternatives considered

- **Restore the original `bool should_defer_activate(focused)` semantics.**
  Rejected: it preserves the convoluted deferred-reactivation indirection
  (flag set on `activate`, committed on a later `done` boundary) when the
  active→active case is better handled directly and explicitly at `done`.
- **Delete re-activation handling entirely.** Rejected: re-activation while
  focused is a real, observable case (clicking between fields in one window);
  it genuinely needs a Panel re-anchor and indicator re-evaluation for the new
  caret, even though the grab and composition are preserved.
- **Rebuild the grab on REACTIVATE** (as the dead `handle_reactivation` did).
  Rejected: the grab is per-input-method, not per-field; rebuilding churns key
  state for no benefit. Matches the prior *runtime* behaviour (which rebuilt
  nothing, since `handle_reactivation` never ran).

## Consequences

- Positive: focus classification is one pure, unit-tested function; the
  indicator re-reveals correctly on reactivation; the phase no longer gets stuck at
  `ACTIVATING` after a re-activation; ~60 lines of unreachable code removed; a
  previously orphaned test now runs in CI.
- Trade-off: introduces an explicit `activate_seen` fact, which is a small step
  away from the pure-derived ADR-0003 ideal — accepted as pragmatic and clearer
  than the dead flag it replaces.
- Verification: the pure classifier is covered by `test_lifecycle`; the
  imperative wiring (grab persistence on reactivation, phase settling) is exercised
  only against a live compositor and must be verified there.
