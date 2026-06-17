# ADR-0034: Dynamic engine capabilities

- **Status**: Accepted
- **Date**: 2026-06-17
- **Deciders**: Project maintainers

## Context

ADR-0031 made language the user-facing switch unit and required engines to
declare their supported languages statically in the manifest. ADR-0033
hardened the language-led tray surface (icon, menu, ID layout) on top of
that assumption.

Static declaration is the wrong abstraction for **schema-driven meta-engines**.
`typio-engine-rime` is the canonical case: one binary that serves Mandarin
(`zh`), Cantonese (`yue`), Wu (`wuu`), Min Nan (`nan`), Hakka (`hak`), and
more, depending entirely on which schemas the user installed under
`~/.local/share/typio/rime/`. Forcing the manifest to declare one fixed
language set undersells the engine for users with non-Mandarin schemas and
misleads users when the manifest declares a language the local install
cannot actually service.

The mismatch produces two concrete UX failures:

1. **The language menu hides the engine.** ADR-0033's rule "a multi-language
   engine appears only under its first matching language submenu" (chosen to
   avoid duplicate dbusmenu IDs) means rime would show under 中文 but not
   under 粵語, even when rime legitimately serves both.
2. **Declared languages drift from reality.** A user installs jyutping but
   the manifest still says `["zh"]` — 粵語 never appears. Or the manifest
   says `["zh", "yue"]` but the user uninstalled jyutping — activating 粵語
   now fails silently.

## Decision

Treat the language set as **runtime data**, owned by the engine itself.
Layer three declaration mechanisms, each able to refine the previous:

### L1 — Static manifest (default, unchanged)

The manifest's `languages` array remains the initial value, registered via
`typio_registry_set_engine_languages` at load time. It is the floor: a
conservative default that holds even before the engine worker has a chance
to report its actual capabilities. Simple engines that do not implement
runtime declaration keep working with no code change.

### L2 — Runtime self-report (new)

libtypio's `typio_registry_set_engine_languages` is now callable any number
of times; each call **replaces** the engine's declared language set and
fires `typio_instance_notify_languages_changed`. The host subscribes via
`typio_instance_set_languages_changed_callback` and rebuilds derived
surfaces (tray menu, persisted-active-language validation, IPC
`language.list`).

This lets a worker process introspect its actual configuration at startup
and report the real capability set. For rime: enumerate
`~/.local/share/typio/rime/*.schema.yaml`, map each schema to a language
via config, and call `typio_registry_set_engine_languages` with the union.

### L3 — User override (deferred)

A user-side config escape hatch ("force rime to declare only `zh` even
though jyutping is installed") is **not** implemented in this ADR. If the
L1 + L2 layered model proves insufficient, L3 lands as a separate change;
the wiring points already exist (any caller can `set_engine_languages`
again with the desired set).

### Menu display rule (amends ADR-0033)

The "first matching language only" rule is reversed: **a multi-language
engine now appears under each declared language submenu**, with composite
dbusmenu IDs so each `(language, engine)` pair has a distinct address.

The composite formula:

```c
TYPIO_TRAY_SECTION_ENGINE + lang_idx * TYPIO_TRAY_ENGINE_MAX + engine_idx
```

Range with current caps (16 languages × 16 engines): 3000..3255. The click
handler decodes `lang_idx = (id - 3000) / 16` and `engine_idx = (id - 3000) % 16`,
switches to the parent language first, then activates the engine.

This lets the user pick "rime under 粵語" vs "rime under 中文" as distinct
menu targets, and the host honours the chosen language rather than guessing
from the engine's first declared language.

### Active-language invalidation

When an engine drops a language from its declared set (a schema was
uninstalled), the active language may become unsupported. The host's state
controller detects this via `typio_state_controller_refresh_language` on
the languages-changed broadcast and re-resolves the status icon; libtypio
itself handles the registry-level fallback (the language slot persists but
the engine slot deactivates, matching the layout-only language behaviour
from ADR-0031).

## Alternatives considered

- **Drop the static manifest field entirely; require runtime declaration
  for all engines.** Rejected: simple engines (basic composer for `en`)
  would have to gain a worker protocol extension and self-introspection
  just to declare one language. L1 as a baseline keeps the engine-author
  bar low; L2 layers on top for engines that need it.

- **Host-side schema discovery (typio-linux scans rime's schema directory
  on rime's behalf).** Rejected: the host should not know about a specific
  engine's on-disk layout. The engine is the only party that can
  truthfully report its own capabilities.

- **User config override (L3) as the only mechanism.** Rejected: pushes
  all the complexity onto the user, who has to track which schemas are
  installed and write the matching config. The runtime self-report is the
  correct default; L3 remains available as a future escape hatch.

- **Per-schema virtual engines (each rime schema becomes its own engine
  entry).** Considered seriously; it would fit the existing language
  model perfectly with no libtypio changes. Rejected for now because the
  engine-protocol extension required to share one process across multiple
  virtual engine entries is non-trivial, and the composite-ID approach in
  this ADR captures most of the UX win at a fraction of the cost. The
  per-schema model remains open as a future direction if the
  meta-engine-with-shared-process pattern becomes common.

## Consequences

- Positive: rime's full multi-language capability is now expressible.
  Installing jyutping makes 粵語 appear in the language menu automatically
  (once the rime worker wires up the L2 call in follow-up work).
- Positive: the declared language set is always truthful — engines report
  what they can actually do right now, not what the manifest guessed at
  build time.
- Positive: the menu correctly distinguishes "rime under 中文" from "rime
  under 粵語" as separate click targets.
- Trade-off: the dbusmenu ID space now uses a composite formula instead of
  flat indexing. The menu model is unit-tested against the formula so a
  future change to the stride breaks loudly.
- Trade-off: the host has to handle a new edge case — the active language
  becoming unsupported mid-session. The state controller's refresh path
  covers it, but it is new behaviour to verify.
- Negative (accepted): the worker-side protocol message that lets an
  out-of-process engine trigger the L2 call ("capabilities changed") is
  **not** added by this ADR. Until it lands, the host-side
  `typio_registry_set_engine_languages` call is reachable only from
  in-process code or via the engine loader at registration time. Wiring
  rime's worker to actually report dynamic languages requires extending
  the engine protocol; tracked as follow-up work in the engine-protocol
  spec.

## Related

- [ADR-0031](0031-language-first-switching-surface.md) — language as the
  switch unit; the static declaration model this ADR extends.
- [ADR-0033](0033-language-led-tray-surface.md) — language-led tray icon
  and menu; the "first matching language only" rule this ADR reverses.
- [ADR-0028](0028-direct-ipc-engine-workers.md) — out-of-process engine
  workers; why the runtime self-report path needs a protocol extension to
  reach rime.
