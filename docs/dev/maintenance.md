# Developer Maintenance Manual

## Purpose

This manual documents the maintenance rules for typio-wayland's Wayland keyboard pipeline. Its goal is to keep the implementation safe, robust, predictable, and easy to extend without reintroducing stale-key bugs or asymmetric key sequences.

Use this document together with the [Wayland Input Method Protocol](../explanation/wayland-input-method.md) and [Timing Model](../explanation/timing-model.md) when changing keyboard handling, engine interaction, or focus lifecycle code.

## Design Goals

The keyboard path must preserve these properties:

- every forwarded key press must eventually produce exactly one forwarded release
- synthetic releases must never leak into a later activation
- startup suppression must distinguish between stale held keys and newly pressed keys
- key tracking state must be reset on activation boundaries
- engine-facing events and app-facing events must stay symmetric unless a rule explicitly says otherwise
- modifier transitions must not rewrite the press/release sequence of unrelated non-modifier keys

## Activation Lifecycle

The current lifecycle is:

1. `zwp_input_method_v2.activate`
2. session reset
3. `done`
4. `focus_in`
5. old keyboard grab destroyed if present
6. new keyboard grab created

If `activate` arrives while the current session is already focused, treat it as a deferred reactivation request:

1. keep the current grab active long enough to finish any in-flight key cycle
2. record pending reactivation state only
3. wait for the matching `done`
4. recreate the keyboard grab at that commit point

Rule: do not move the frontend out of `active` just because a repeated `activate` arrived for an already focused session.

On deactivation:

1. forwarded keys are force-released to the virtual keyboard
2. modifier carry may preserve the last compositor-reported modifier mask across the boundary for the newly focused client
3. repeat is cancelled
4. on focus loss in `done`, the keyboard grab is destroyed
5. per-key tracking state is reset

Rule: no `TypioKeyTrackState` value may survive from one activation to the next.

## Key Tracking State Machine

`TypioKeyTrackState` lives in `src/frontend/internal.h`. Per-key ownership also tracks a `key_generation`: a key cycle belongs to the current grab only if the daemon observed its press in the current generation.

States:

- `TYPIO_KEY_TRACK_IDLE`: no outstanding tracked ownership
- `TYPIO_KEY_TRACK_FORWARDED`: press was forwarded to the application
- `TYPIO_KEY_TRACK_APP_SHORTCUT`: non-modifier shortcut key bypassed the engine and was forwarded directly to the application
- `TYPIO_KEY_TRACK_RELEASED_PENDING`: the daemon already sent a synthetic release and must consume the physical one
- `TYPIO_KEY_TRACK_SUPPRESSED_STARTUP`: dormant — no longer entered by routing (stale presses are dropped by the grab-generation fence, not suppressed at the press path); the state and its release handler are retained only as a defensive cleanup path
- `TYPIO_KEY_TRACK_VOICE_PTT`: voice push-to-talk is actively held
- `TYPIO_KEY_TRACK_VOICE_PTT_UNAVAIL`: voice push-to-talk binding was pressed, but voice is unavailable

Maintenance rules:

- only `TYPIO_KEY_TRACK_FORWARDED` may produce forwarded releases during normal key-up
- `TYPIO_KEY_TRACK_APP_SHORTCUT` must forward both press and release without involving the engine
- `TYPIO_KEY_TRACK_RELEASED_PENDING` must be cleared by the matching physical release or by activation reset
- `TYPIO_KEY_TRACK_SUPPRESSED_STARTUP` may forward only a cleanup release
- voice tracking states are internal consume paths and must not leak VK events
- do not synthesize releases for forwarded non-modifier keys just because Ctrl, Alt, or Super changed
- a release for a key whose press never reached the daemon in the current generation is an orphan release and must be consumed locally, not sent to the engine or app

Do not overload tracking state names with routing semantics. Final routing decisions now live in `TypioWlKeyDecision { action, reason }`; tracking states only record press/release ownership across lifecycle boundaries.

## Startup Guard Rules

Stale **presses** are not suppressed by a time/epoch window. A key press is trusted as genuine user input the moment it arrives; a key whose generation does not match the active grab is dropped by the grab-generation fence (see [Timing Model §One epoch fence](../explanation/timing-model.md#one-epoch-fence)). Do not reintroduce a dispatch-window press filter — it cannot distinguish a compositor re-send from a real keystroke and silently eats the first key after every grab rebuild (notably on the reactivation churn that terminals and tmux produce).

The startup guard's only remaining role is to bound **orphan-release cleanup**: `typio_wl_startup_guard_is_in_guard_window()` (using the grab's `created_at_epoch`) marks the brief window in which a release for a key that was held before the grab existed may be forwarded to the virtual keyboard so the focused client is not left with a stuck key.

The startup guard does not own activation-boundary VK handoff decisions. Orphan release cleanup and carried modifier behavior belong to `boundary_bridge.*`.

## Lifecycle Cleanup Boundary

`key_tracking.c` centralizes the lifecycle-only bulk operations on key state:

- reset all tracked keys when a grab is created or destroyed
- mark forwarded keys as `RELEASED_PENDING` only when the daemon explicitly performs boundary cleanup during deactivation or grab teardown

Rule: bulk conversion of forwarded keys is a lifecycle operation, not a normal modifier-path operation. If a change needs to rewrite many key states at once, it belongs at a focus/grab boundary and should be implemented through this helper layer.

`boundary_bridge.*` owns the policy decisions for short-lived bridge behavior at activation boundaries:

- whether an orphan non-modifier release should be forwarded as cleanup
- whether carried virtual-keyboard modifiers must be reset
- whether deactivation may carry the compositor's last modifier mask across the boundary

Rule: keep boundary handoff decisions in `boundary_bridge.*`; callers may execute the resulting VK or lifecycle action, but should not duplicate the conditions inline.

Rule: virtual keyboard output is only valid for keys the daemon actually owns in the current generation. Do not use VK events to "repair" a key cycle whose press did not pass through the daemon.

Exception: at the `deactivate` boundary, the daemon may carry the last compositor-reported modifier mask to the virtual keyboard so a held Ctrl/Alt/Super can still affect the newly focused client. That carried VK modifier state is a short-lived bridge only and must be cleared before the next activation starts.

Exception: an orphan `Enter` release may be forwarded as activation-boundary cleanup even without shortcut modifiers, because the application must not be left with a stuck app-facing `Enter` press.

## Modifier Truth

Effective event modifiers are centralized in `modifier_policy.*`.

Rule:

- owned generations trust the daemon's physical modifier tracking
- not-yet-owned generations may inherit Ctrl/Alt/Super from `xkb_state`
- Caps Lock and Num Lock come from `xkb_state`
- do not re-implement modifier fallback rules inline in `wl_keyboard.c`

## Shortcut Policy

The normative shortcut rules live in [Timing Model](../explanation/timing-model.md). For maintenance work, treat them as non-negotiable invariants:

- shortcut bypass policy belongs to the Wayland frontend, not per engine
- shortcut press/release handling must remain symmetric
- modifier-only compositor shortcuts such as `Ctrl+Shift` stay transparent
- cleanup rewrites belong at lifecycle boundaries, not in the normal key path

## Safe Change Checklist

Before merging keyboard-path changes, verify:

- activation and deactivation do not leave per-key state behind
- repeated `activate` during an already focused session does not interrupt an in-flight key press/release pair
- key repeat cancellation still works when modifiers change
- forwarded modifier shortcuts still reach applications
- engine-only handled keys do not leak releases to applications
- startup Enter suppression does not emit a lone app-facing release
- engine switch (Ctrl+Shift, tray menu, IPC) clears composition, preedit, and candidate panel before the new engine activates
- tests cover the new state transition or lifecycle boundary

Before merging runtime-scheduling changes, verify:

- virtual-keyboard keymap deadlines still shorten the poll timeout while waiting for `ready`
- config reload bursts are debounced and file replacement re-arms the watch
- status and tray D-Bus dispatch are bounded per event-loop tick
- voice reload is deferred while recording or processing and applied afterward
- failed keyboard or voice engine switches restore the previous active engine in the same category

## Required Tests

At minimum, keyboard-path changes should keep these areas covered:

- startup guard orphan-release window
- boundary bridge policy
- repeat cancellation helper logic
- activation-boundary reset of key tracking state
- orphan release cleanup
- deferred reactivation commit rules

If a change touches `wl_keyboard.c` and cannot be covered by an existing helper test, add a new helper or state-policy test rather than relying only on manual testing.

## Debugging Workflow

When investigating swallowed or duplicated keys:

1. enable debug logging
2. capture the sequence of `press`, `release`, `modifiers`, and focus events
3. record the per-key state transition
4. verify whether the key crossed an activation boundary
5. check whether the key was classified as stale, Enter-guarded, forwarded, or force-released

Typical signatures:

- first press after reactivation disappears: likely stale `RELEASED_PENDING` state
- app receives release without prior press: wrong suppression-state release handling
- Ctrl-based shortcut behaves oddly: main-path key sequence is being rewritten instead of only syncing modifiers
- app keeps repeating a shortcut letter after reactivation: `boundary_bridge.*` likely missed an orphan-release VK cleanup decision
- a held Ctrl/Alt/Super stops affecting shortcuts after grab recreation: the new grab saw the modifier only via XKB/modifier events, not a fresh key press; physical modifier state must be resynchronized from XKB on modifier updates so later shortcuts still inherit the held modifier
- a held Ctrl/Alt/Super stops affecting the newly focused client after deactivation: verify whether the last compositor modifier mask was carried across the boundary and later reset before the next activation

## Long-Term Performance

The Wayland frontend is designed for sustained operation (days to weeks). Changes that affect memory pressure or allocation patterns must consider multi-day runtime behavior.

### Font Cache Management

`text_shaper.c` maintains three caches:

- **Font-object cache** (`FONT_OBJ_CACHE_CAP` = 64) — `FcPattern` → `TypioFluxFont`.
- **Font-file cache** (`FONT_FILE_CACHE_CAP` = 32) — `(path, weight)` → `FcPattern` + file handle.
- **Fallback-font cache** (`FALLBACK_FONT_CACHE_CAP` = 16, `src/ui/panel/fallback_cache.c`) — coverage-keyed LRU. Each resolved fallback font remembers the `FcCharSet` it covers; a later text reuses it when its codepoints are a subset (`FcCharSetIsSubset`). Keyed on coverage, **not** the text string, so a long CJK session — an unbounded stream of distinct phrases served by one font — resolves ~once per script instead of re-running `FcFontSort` per phrase. (The earlier text-keyed, cap-then-stop cache degraded to a ~0 % hit rate and re-ran the resolver on every composition; unit-tested in `tests/test_fallback_cache.c`.)

There is also a **glyph atlas** (`src/ui/panel/text_shaper.c`, `glyph_atlas_*`): a
single persistent R8 texture holding every rasterised glyph, keyed
`(font_id, glyph_id)` and packed by the skyline allocator in
`src/ui/panel/glyph_pack.c` (ADR-0012). It is bounded (one ~4 MiB texture) and the
reason candidate paging no longer uploads textures on the hot path. It is keyed
on `font_id`, so it is dropped alongside the font objects in
`typio_text_shaper_purge_font_caches()` and rebuilt lazily.

These caches are warmed on first use and survive for the lifetime of the process. To prevent unbounded Fontconfig growth:

- **`typio_text_shaper_purge_font_caches()`** must be called at safe idle boundaries. Currently this happens on every config reload (`runtime_config.c`). It drains the font caches **and** frees the glyph atlas (both must be idle — it is called where the GPU is not mid-frame).
- Do not call `FcFini()` during an active composition or while the GPU renderer is holding font atlas references.
- If you add a new cache in `text_shaper.c`, add a corresponding `*_cache_clear()` helper and wire it into `typio_text_shaper_purge_font_caches()`.

### Composition Pipeline

Candidate data travels through three ownership layers:

1. **Engine** (RIME) owns the raw candidate list.
2. **libtypio** (`content.rs`) copies into `TypioComposition`.
3. **typio-wayland** (`wl_input_method.c`) copies into `TypioCandidateList` snapshot.

To reduce churn:

- libtypio short-circuits when only `selected`/`cursor_pos` changed (selection-only navigation).
- typio-wayland short-circuits the snapshot deep-copy when content is identical (`candidate_snapshot_equal_content`).
- When extending the composition or snapshot structs, preserve the equality helpers. A missing field in the equality check causes unnecessary deep-copies; an incorrect check causes stale UI.

### External Factors

- **RIME userdb bloat** is outside typio's control. If sluggishness persists across daemon restarts, advise users to run `rime_deployer --build` or trim their user dictionary.
- **Compositor memory pressure** (Mesa/AMDGPU caches, SHM pools) is not managed by typio. The font purge is the only renderer-side cache we control.

### Monitoring

When investigating long-term slowdown:

1. Check whether the symptom survives a daemon restart. If yes, suspect RIME userdb or compositor state.
2. If the symptom disappears after restart but reappears over time, it could be a CPU-side cache degradation **or** an off-CPU stall. **Triage CPU vs off-CPU first** (next subsection) — do not assume it is font lookups or `candidate_snapshot_assign` allocations until on-CPU profiling confirms a CPU hotspot.
3. Verify that `typio_text_shaper_purge_font_caches()` is being invoked (debug log or breakpoint in `runtime_config.c`).

### Diagnosing event-loop stalls with system tools

A "laggy Panel / candidate switching" report is usually an **off-CPU** stall
(blocked on the compositor or GPU), not a CPU hotspot. Profile the **already
laggy live process** — do not restart it, the degraded state is the evidence.
Reading code and guessing is how the 2026-05 investigation first blamed the
font path; only measurement found the real cause (ADR-0010).

The environment constraints matter: `kernel.perf_event_paranoid = 2` blocks
tracepoints/BPF (so `perf trace` won't load), and `yama/ptrace_scope = 1`
blocks `strace`/`gdb` attach to a non-child unless run as root. `perf record`
uses `perf_event_open` (not ptrace) and works on your own process.

1. **CPU vs off-CPU.** `perf record -p <pid> -g --call-graph dwarf -F 999
   sleep 30` while actively reproducing. Few samples over a busy window
   (e.g. ~30 in 30 s) ⇒ the process is **off-CPU / blocked**, not CPU-bound;
   the font and RIME paths are *not* the bottleneck. Many samples ⇒ read the
   call graph for the CPU hotspot.
2. **Where it blocks — kernel view (cheap, ambiguous).** As root,
   `cat /proc/<pid>/stack` in a loop. Caveat: the idle 100 ms loop `poll` and a
   `poll` *inside* a present both show as `do_sys_poll` — the kernel stack
   **cannot distinguish them**, so "all poll" is not a conclusion.
3. **Where it blocks — user-space view (decisive).** Poor-man's profiler:
   `for i in $(seq 1 80); do gdb -p <pid> -batch -ex 'set pagination off' \
   -ex 'thread apply all bt 12'; done` (root, while reproducing). The process
   runs freely between attaches, so this samples real wall-clock blocking.
   Aggregate the non-idle frames. The 2026-05 signature was the main thread in
   `panel_surface_present → flux_frame_present → vkQueuePresentKHR →
   wsi_wl_swapchain_queue_present` ~86 % of the time — a FIFO present blocking
   on compositor buffer release (fixed by ADR-0010's non-blocking present).

A laggy Panel had **layered** causes; do not stop at the first one (the
2026-05 investigation called "fixed" twice before the real cause). Confirmed
chain:

- **Present-side FIFO block** — main thread ~86 % in `vkQueuePresentKHR`
  waiting on the compositor. Fixed by non-blocking present
  ([ADR-0010](../adr/0010-non-blocking-candidate-popup-present.md)). Real, but
  the lag returned.
- **Per-colour glyph duplication** — a *real* contributor, fixed by
  colour-independent R8 coverage textures + draw-time tint
  ([ADR-0011](../adr/0011-colour-independent-coverage-glyphs.md)). It halved
  the textures but did **not** remove the synchronous upload below; the lag
  returned, so this was not the root.
- **Synchronous per-text-run texture upload** — a *real* anti-pattern, but
  **not** the root cause of the recurring lag (see the next bullet). A gdb
  profile while paging once caught `typio_flux_fill_layout → build_layout_image
  → flux_image_create → submit_one_shot_and_wait → vkWaitForFences` (both since
  removed/renamed):
  `build_layout_image` rasterised a whole text run into one texture and uploaded
  it synchronously. Removed by a **shared glyph atlas** — rasterise each glyph
  once, reference sub-rects, upload-once-per-glyph
  ([ADR-0012](../adr/0012-glyph-atlas-shared-texture.md)). Worth doing for
  bounded GPU memory and no per-run churn — but after it shipped the lag *still*
  returned, which is what finally exposed the true cause.
- **Per-candidate-page swapchain rebuild** — the *actual* root cause
  ([ADR-0013](../adr/0013-grow-only-popup-swapchain.md)). Diagnosed from the
  **live process**, not the source: `nm /proc/<pid>/exe` on the running
  (deleted) inode proved the atlas was live and `build_layout_image` gone, yet
  the lag persisted — so the upload theory was wrong. An on-CPU `perf` then
  showed **<1 % on-CPU** (⇒ a *blocking* wait, not computation), with the few
  on-CPU samples in `wsi_wl_display_init → wl_display_roundtrip_queue →
  wsi_wl_surface_get_formats/get_present_modes` — i.e. **swapchain recreation
  during normal use**. `ensure_fx_surface` rebuilt the swapchain whenever the
  Panel width changed (`flux_surface_resize` = `vkDeviceWaitIdle` + teardown +
  WSI roundtrips), and **every candidate page changes the width**, so paging ran
  a full GPU stall + swapchain rebuild per keypress on the IME loop. No prior
  ADR (0010 present mode, 0011/0012 textures) touched this path, which is why
  the lag returned after each. Fixed by a **grow-only, quantised swapchain
  buffer** cropped to content via `wp_viewport_set_source` — steady-state paging
  rebuilds nothing.
- **GPU memory (~192 MB)** looked alarming but was a *bounded plateau* of
  flux's 64 MiB pool blocks (`/proc/<pid>/fdinfo` `drm-total-system0` stayed
  flat under load), driven by the per-run texture churn — not a leak and not
  the cause. The atlas (one bounded 4 MiB texture) removes it.

**Rules of thumb:**
- Any GPU present on the IME loop must be non-blocking (ADR-0010) and bounded
  on acquire (ADR-0006). An on-demand surface must not use FIFO/vsync — FIFO
  assumes a continuous per-vblank loop and blocks on buffer release an idle
  surface never gets.
- **Never "new text run ⇒ new texture ⇒ synchronous upload."** Glyphs go in a
  shared, persistent atlas, rasterised once and referenced by sub-rect
  (ADR-0012); colour stays a draw-time tint, never baked per texture (ADR-0011).
  A synchronous `flux_image_create`/`flux_image_update_region` on the Panel
  draw path (look for `submit_one_shot_and_wait → vkWaitForFences`) is the
  regression to watch for.
- A symptom that **survives swapping the graphics library** is architectural —
  in how the frontend drives the GPU, not in the library. Don't chase
  library-specific causes when the report spans renderers. (This pointed at
  per-page *swapchain rebuild* — ADR-0013 — once on-CPU profiling ruled out the
  upload theory.)
- **Triage blocking vs computation before reading code.** A near-empty on-CPU
  `perf` capture (≪1 % on-CPU during the lag) means the loop is *blocked*, not
  busy — switch to a **wall-clock blocked-stack sampler**
  (`gdb -p <pid> -batch -ex bt` in a loop) which captures syscall/fence/roundtrip
  stacks on-CPU `perf` cannot see. On-CPU `perf` can neither find nor prove the
  fix of a blocking stall.
- **Diagnose the live process, not the source tree.** `nm /proc/<pid>/exe` on
  the running (possibly `(deleted)`) inode tells you which fixes are actually
  *running*. It disproved the upload theory (atlas present, lag persisted) and
  saved chasing already-fixed code.
- **Never rebuild a swapchain on a per-keystroke size change.**
  `flux_surface_resize` is `vkDeviceWaitIdle` + swapchain teardown/rebuild + WSI
  compositor roundtrips — all synchronous on the IME loop. A Panel whose width
  tracks content must use a **grow-only buffer cropped by
  `wp_viewport_set_source`** (ADR-0013), not a resize per frame.
- A precise *behavioural* repro (what triggers it vs what doesn't) narrows the
  search faster than broad profiling: "navigation lags, typing doesn't,
  selection stays correct" pointed straight at per-page GPU upload work.

## Documentation Update Policy

Any change that modifies one of these must update this manual:

- `TypioKeyTrackState`
- startup suppression policy
- activation/deactivation ordering
- synthetic release behavior
- keyboard repeat ownership
- runtime event-loop scheduling
- config-watch reload behavior
- voice reload deferral
- font cache management or cache purge behavior
- composition pipeline equality short-circuits

This keeps maintenance knowledge in-repo instead of in commit history only.
