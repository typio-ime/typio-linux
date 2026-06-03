# Wayland Input Method Protocol

`typio` is a Wayland-native input method. Every key the user presses, every preedit string shown, and every positioned Panel update travels through the `zwp_input_method_v2` family of unstable protocols. This document maps how the daemon implements those protocols, what workarounds it applies to the unstable surface, and where the detailed rules live.

This is a **connective-tissue** document: it does not replace the protocol specification, the source-code comments, or the deep-dive timing model. It exists so a reader can answer "how does typio handle X?" in one stop rather than grepping across `wl_input_method.c`, `wl_keyboard.c`, and five other explanation files.

For the protocol specification see the upstream [wayland-protocols `input-method-unstable-v2.xml`](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/unstable/input-method/input-method-unstable-v2.xml). For timing, lifecycle, and daemon-resilience rules see [Timing Model](timing-model.md).

## Protocol Stack

The daemon binds five Wayland protocol layers. The input-method layer is the one the daemon *implements*; the others are dependencies or peers.

### Compositor-provided interfaces (daemon is the consumer)

| Interface | How the daemon uses it |
|---|---|
| `zwp_input_method_manager_v2` | `get_input_method()` → receives the `zwp_input_method_v2` object the daemon listens on |
| `zwp_input_method_keyboard_grab_v2` | `grab_keyboard()` → receives raw key/modifier/keymap events for the focused input context |
| `zwp_input_popup_surface_v2` | `get_input_popup_surface()` → positions the Panel near the cursor |
| `zwp_text_input_manager_v3` | The daemon does not bind this directly, but relies on the compositor exposing it so client applications can participate in the text-input session |
| `wl_compositor` | `create_surface()` → creates the Panel `wl_surface` backed by a Vulkan swapchain |
| `wl_output` | Listens to `scale` events so the Panel renders at the correct DPI for each monitor |

### Client-provided interfaces (daemon depends on their presence)

| Interface | Role |
|---|---|
| `zwp_text_input_v3` | The application side (Firefox, terminal, chat) enables text input, sends surrounding text and content type to the compositor, which forwards it to the daemon through `zwp_input_method_v2` |

### Daemon-provided interfaces

| Interface | Role |
|---|---|
| `zwp_input_method_v2` | Listens for `activate`, `deactivate`, `done`, `surrounding_text`, `text_change_cause`, `content_type`, `unavailable`; sends `set_preedit_string`, `commit_string`, `commit`, `delete_surrounding_text` |
| `zwp_virtual_keyboard_v1` | Forwards unhandled keys back to the compositor as synthetic press/release events; managed by the vk-bridge module for keymap handoff and readiness gating |

## Event Handlers: events become facts

Every `zwp_input_method_v2` event does one thing — **record a fact**. No handler mutates a phase, rebuilds a grab, or decides a transition; that all happens when the per-step diff runs (at `done` and once per event-loop iteration). This is what keeps the handlers trivial and the lifecycle in one place.

### `activate` / `deactivate`

Record the pending focus fact (`active = true` / `false`) for the current session, creating the session if none exists. `activate` additionally records an `activate_seen` fact for the current `done` batch (and hides any stale positioned indicator from the prior activation). They make no transition decision themselves — that happens at `done`. `activate_seen` is what lets `done` tell a genuine (re)activation apart from a plain text-state update; see [ADR-0018](../adr/0018-focus-transition-classification.md).

### `surrounding_text`, `text_change_cause`, `content_type`

Record client editing-context facts (buffered during the `done` batch). These are hints, not commands; the engine may ignore them or use them to improve prediction.

### `done`

The compositor's double-buffer commit point, and where the per-step diff runs.

**Why double-buffering?** The `zwp_input_method_v2` protocol sends a batch of events (`activate`, `deactivate`, `surrounding_text`, `content_type`, …) followed by a single `done`. Events in the batch are provisional — they record *facts* into a pending buffer, but no action is taken until `done` commits the batch atomically. This is the same pattern as `wl_surface::commit`: stage changes, then apply them all at once.

**Why not react per-event?** Two scenarios demonstrate the problem:

1. **Cancelled activation.** A UI flicker can produce `activate` → `deactivate` within one batch. Per-event handling would build the keyboard grab, call engine `focus_in`, show the indicator — then immediately tear it all down. With `done`-time reduction, the two facts cancel out: `was=false, now=false` → `NONE`, zero work done.

2. **Reactivation.** Clicking from one text field to another inside the same window produces `activate` while already active (no intervening `deactivate`). Per-event handling would build a new grab on `activate`, destroying the existing one mid-composition. With `done`-time reduction: `was=true, now=true, activate_seen=true` → `REACTIVATE` — the grab and composition are preserved, only the Panel anchor is refreshed.

**Steps at `done`:**

1. **Serial increment**. `im_serial++`. The serial is the count of `done` events received; it is the commit serial for every `zwp_input_method_v2_commit()` call.
2. **Apply facts**. The buffered `surrounding_text`, `content_type`, `text_change_cause`, and `active` facts become current atomically.
3. **Classify the state change.** The focus facts are reduced by the pure `typio_wl_lifecycle_classify_done(was_active, now_active, activate_seen)` into one action — `ACTIVATE`, `DEACTIVATE`, `REACTIVATE`, or `NONE` — which selects `transition_to_active` / `transition_to_inactive` / `transition_to_reactivate` / no-op. The classifier lives in `engine/lifecycle_policy.c` and is unit-tested (`tests/test_lifecycle.c`). A `NONE` that still finds a non-routable grab triggers the stale-grab recovery. See [ADR-0018](../adr/0018-focus-transition-classification.md).

   > Note: the focus path still carries an explicit `lifecycle_phase`; the fully *derived* reduce+diff model of [ADR-0003](../adr/0003-session-controller-reduce-diff.md) is not yet realised here, and `classify_done` is a small step toward it.

### `unavailable`

Another input method has taken the seat. The daemon sets `frontend->running = false`, logs a warning, and stops.

## Commit Serial and the Chokepoint

`zwp_input_method_v2` requires a serial on every `commit()` call. The serial must match the most recent `done` event. Before the first `done`, the serial is 0.

The daemon treats serial 0 as a **write barrier**: `typio_wl_commit()` refuses to send `set_preedit_string` or `commit_string` when `im_serial == 0`. This prevents a race where the IME stages preedit text before the compositor has established the input-method connection, which would cause the compositor to silently drop the staged text without error.

```c
if (frontend->im_serial == 0) {
    /* Skip: compositor has not sent done yet. */
    return;
}
zwp_input_method_v2_commit(im, frontend->im_serial);
frontend->last_committed_serial = frontend->im_serial;
```

`last_committed_serial` is a diagnostic breadcrumb. It is also the hook for future reconnect work: if the compositor restarts and the serial resets, the daemon can detect the discontinuity.

This chokepoint is the single place where every protocol write is validated. All preedit updates, commit strings, and Panel geometry requests funnel through `typio_wl_commit`.

## Keyboard Grab Lifecycle

The grab and its keymap handshake are **one resource** (`absent → needs_keymap → ready → broken`) that the diff creates and tears down; the timing rules are in [Timing Model](timing-model.md).

Briefly:
- Each grab incarnation has an **epoch**. Every recorded key fact carries the epoch it arrived under, and a key whose epoch ≠ the current grab epoch is dropped at routing — this is the single fence that replaces the old generation arrays, `suppress_stale_keys`, and `created_at_epoch`.
- When a grab is rebuilt (focus-in, re-activate, resume, reconnect), the compositor may re-send already-in-flight keys; the epoch fence discards them.
- Unhandled keys are forwarded as original press/release pairs through the virtual keyboard.
- Modifier state is synced separately; modifier changes do not synthesise releases for unrelated non-modifier keys.
- Two fail-safe backstops remain: an emergency-exit shortcut and a rejected-press-streak threshold, both releasing the grab.

## Virtual Keyboard Forwarding (`vk_bridge.c`)

`zwp_virtual_keyboard_v1` is the daemon's output path for keys the engine declined (`TYPIO_KEY_NOT_HANDLED`). The vk-bridge module manages:

- **Keymap handoff** — when the compositor delivers a new keymap on the grab, the vk must receive the same keymap before forwarding keys, or modifier mappings will mismatch.
- **Readiness gating** — vk forwarding is blocked until the keymap is confirmed, preventing modifier-sync errors during activation handshakes.
- **Fail-safe downgrade** — if vk health degrades (keymap deadline missed, compositor stalls), the daemon falls back to local key handling rather than forwarding broken state.

## Re-activate while focused: re-anchor, keep the grab

A subtle protocol behaviour: the compositor may send `activate` while the daemon is still focused (e.g. the user clicked from one text field to another inside the same window). Treating this as a full `deactivate` → `activate` cycle would tear down the grab, lose the preedit round-trip, and interrupt typing.

The `activate_seen` fact makes this case explicit without that cost. At `done`, `classify_done(was=true, now=true, activate_seen=true)` returns `REACTIVATE`, which runs `transition_to_reactivate`: the keyboard grab and the engine's input context are **left intact** (they belong to the same input-method, not the field), and only the *position-sensitive* state is refreshed — the Panel anchor is reset. The indicator is **not** re-shown on reactivation: the engine state has not changed across a field switch, so the old indicator (already hidden by `activate`) should not carry over. Only genuine state changes — `ACTIVATE`, deliberate engine/mode switches — announce via the indicator. A `done` with no `activate` this batch (`activate_seen=false`) classifies as `NONE` and changes nothing, so plain text-state updates during composition never disturb the grab. See [ADR-0018](../adr/0018-focus-transition-classification.md).

## Resume and silent grab loss

System suspend is invisible to the Wayland protocol: no `deactivate` before sleep, no guaranteed `activate` after wake; a held modifier may be stuck and the grab may be silently dead on wake. The compositor can also drop the grab with no event at all (restart, bug, race).

How much of this the per-step diff handles depends on whether `observe()` can *see* it — and `observe()` reads resource *presence*, not *liveness*:

- **Suspend.** A grab dead across suspend leaves a *live proxy*; `observe()` reports it healthy, so the diff alone is blind. A resume **detector** (logind `PrepareForSleep` plus a `CLOCK_BOOTTIME` vs `CLOCK_MONOTONIC` gap heuristic) records facts: it invalidates the grab epoch and drops the compositor-visible preedit, then lets the step run.
- **Grab object gone.** If the grab *object* is actually absent while `desired` wants it (typically right after one of our scrubs, or a connection drop surfacing as `POLLHUP`), `observe()` sees it and the next per-iteration `diff` rebuilds — no special routine.

In both cases the input context is never `focus_out`'d, so the engine's in-flight composition survives, and the rebuild is the *same* grab build used on first focus.

## Indicator behaviour

The indicator (the transient Panel showing the active engine and mode label) has two show paths, each with different gate semantics:

| Path | Trigger | Gates |
|---|---|---|
| Focus path (`show_indicator_on_focus`) | `ACTIVATE` only | salience (suppress `QUIET` states) + acknowledged-recency (suppress if user typed or saw indicator within the last 3 s) |
| Deliberate-change path (`show_indicator_for_state`) | engine switch, mode change, profile toggle | none — user just acted, always announce |

On `DEACTIVATE`, `transition_to_inactive` hides all Panel UI. On `REACTIVATE`, the indicator stays hidden (it was already hidden by the `activate` handler); the engine state has not changed across a field switch so there is nothing to announce.

The indicator auto-hides after `display.indicator_duration_ms` (default 1500 ms, clamped 100–10000 ms) via a timerfd.

## Known limits: terminal multiplexers

Terminal emulators (foot, Alacritty, kitty, …) register the **entire terminal window** as a single `zwp_input_method_v2` input area. When a terminal multiplexer like tmux or screen splits that window into panes, sessions, or windows, those context switches happen entirely inside the terminal's own rendering — they produce **no Wayland focus events**. From the compositor's perspective, the user never left the same input field.

Consequences for indicator behaviour:

- Switching tmux panes or windows does **not** trigger `activate`/`deactivate`, so the daemon never sees a `REACTIVATE`. Any indicator that was showing stays visible until its auto-hide timer expires.
- There is no way for the input method to detect intra-terminal context switches. This is an inherent limitation of the `zwp_input_method_v2` protocol model, which only knows about compositor-level focus, not application-level editing context.
- The auto-hide timer (`indicator_duration_ms`) is the only mechanism that dismisses the indicator in this scenario. Lowering the duration makes the indicator vanish sooner but shortens the display window for all contexts, including those where the indicator is useful.

This limitation also affects the candidate Panel position: the input-popup surface is anchored to the terminal's cursor rectangle, not to a tmux pane boundary.

## Preedit Round-Trip Optimisation

When the user navigates candidates with `Up`/`Down`, only the `selected` index changes; the preedit text is identical. The daemon detects this in `update_wayland_text_ui`:

```c
update_plan = typio_wl_text_ui_plan_update(session->last_preedit_text,
                                           session->last_preedit_cursor,
                                           new_text, cursor_pos);
```

If `update_plan == TYPIO_WL_TEXT_UI_SYNC_PANEL_ONLY`, the Panel Scheduler
refreshes only the Candidate Panel during the event-loop Panel stage. The
expensive `zwp_input_method_v2.set_preedit_string` → `done` round-trip to the
application is skipped entirely. This avoids composition-update jank in
heavyweight clients like Chrome.

## Source Map

| Protocol object | Source file | Responsibility |
|---|---|---|
| `zwp_input_method_v2` | `src/frontend/input_method.c` | Event handlers (record facts), serial chokepoint |
| Session lifecycle | `src/engine/lifecycle.c` | Phase transitions, resume handling, hard reset |
| Lifecycle policy (pure) | `src/engine/lifecycle_policy.c` | `classify_done`, transition validity, phase predicates — dependency-free, unit-tested |
| Reconciler | `src/engine/reconciler.c` | Observes resources, detects divergence, triggers repair |
| `zwp_input_method_keyboard_grab_v2` | `src/frontend/keyboard.c` | Grab create/destroy, key/modifiers/repeat listeners, emergency exit |
| Key epoch + tracking | `src/input/tracker.{c,h}` | Epoch fence and symmetric press/release |
| `zwp_virtual_keyboard_v1` | `src/input/bridge.c` | Keymap handoff, readiness gating, unhandled-key forwarding, fail-safe downgrade |
| `zwp_input_popup_surface_v2` | `src/ui/panel/surface.c` | Panel geometry, present, retry-on-stall |
| Panel rendering | `src/ui/panel/paint.c` | Vulkan swapchain on `wl_surface` |
| Resume detection | `src/engine/resume.c` | logind + boottime heuristic (records facts) |
| Protocol XML | `protocols/input-method-unstable-v2.xml` | Wayland protocol definition (upstream) |

## See Also

- [Timing Model](timing-model.md) — the derived reduce+diff state model, truth sources, event-loop scheduling, and daemon resilience (suspend/resume, compositor restart, silent grab loss)
- [Panel Appearance](../dev/panel-appearance.md) — Vulkan Panel rendering pipeline
