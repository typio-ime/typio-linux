# ADR-0033: Language-led tray surface (icon and menu)

- **Status**: Accepted
- **Date**: 2026-06-17
- **Deciders**: Project maintainers

## Context

ADR-0031 made **language** the switch unit and reshaped the tray menu, tooltip,
and indicator around it. ADR-0032 layered a language badge underneath the
engine identity on the tray icon. Two follow-up problems surfaced in practice:

1. **The icon did not read as the language.** ADR-0032's precedence chain put
   the engine manifest `icon` (layer 2) above the language badge (layer 4).
   Every keyboard engine that ships an icon — Rime ships `typio-rime-symbolic`,
   the basic engine ships `typio-engine-basic` — therefore suppressed the
   badge, so the icon showed the engine backend instead of the active
   language. ADR-0032's stated goal "the icon *is* the language" only held for
   layout-only languages with no engine at all.

2. **The menu duplicated primary languages.** `typio_language_endonym` matches
   on the primary subtag, so `zh-Hans` and `zh-Hant` both render as "中文".
   With an engine that declares both script variants, the language section
   showed two indistinguishable "中文" entries. ADR-0031 also specified the
   engine list as a flat "within-language choice" beneath the language
   section, which scaled poorly once engines for several languages were
   registered: the user had to correlate two flat lists (languages, engines)
   in their head.

The controller had a second, parallel icon resolution path in
`typio_state_controller_sync` that bypassed the ADR-0032 chain entirely and
fell straight back to the engine manifest icon, so the badge was never
reached at startup even when the chain would have produced it.

## Decision

Make the tray surface language-led in both dimensions: the icon always shows
the active language, and the menu nests engines inside their language.

### Icon precedence (amends ADR-0032's chain)

The status icon is resolved by a single chain in
`typio_state_controller_resolve_status_icon`, most-specific first:

1. `[languages.<tag>].icon` config override — explicit per-language icon
2. **language badge** — rendered text, the floor
3. generic `typio-keyboard-symbolic` — active (language or engine) with no
   icon anywhere
4. `typio-keyboard-off-symbolic` — only when nothing is active

The change from ADR-0032 is that the **engine identity layers are removed
from the tray base icon entirely**. ADR-0032's chain put the engine manifest
`icon` (layer 2) and the dynamic mode/schema icon (layer 1) above the
language badge; every keyboard engine that ships an icon — Rime ships
`typio-rime-symbolic`, the basic engine ships `typio-engine-basic` —
therefore suppressed the badge, so the icon showed the engine backend
instead of the active language. ADR-0032's stated goal "the icon *is* the
language" only held for layout-only languages with no engine at all.

Engine identity is now sourced only from the active language. The resolver
takes no `info` and no `engine_changed` parameter; it consults the registry
for the active language and the controller's `engine_active` flag, nothing
else. A language without a renderable badge (no glyph in the endonym table)
falls through to the generic keyboard icon, never to an engine-supplied
icon. `typio_state_controller_sync` now routes through the same resolver
instead of holding a parallel fallback, so startup sync and live updates
agree.

Two engine-icon pathways that bypassed the resolver are also closed:

- `typio_state_controller_notify_status_changed` no longer promotes the
  mode's `icon_name` to the tray icon; it still stores the mode (label,
  display_label) for the tooltip and broadcasts `TYPIO_STATE_CHANGE_STATUS`
  so the tooltip refreshes.
- `typio_state_controller_notify_status_icon_changed` no longer overwrites
  the resolved icon with the engine-pushed name; it just broadcasts. The
  latest engine-pushed icon remains queryable via
  `typio_instance_get_last_status_icon` for any non-tray consumer.
- The systray adapter's `TYPIO_STATE_CHANGE_STATUS` case no longer calls
  `typio_tray_set_icon(mode->icon_name)`.

The manifest `icon` field, the dynamic `status-icon` callback, and the
`typio_registry_get_engine_icon` API are unchanged. They remain available
for surfaces that legitimately show engine identity: the menu, the
candidate panel, settings UIs, and `typioctl`. The tray base icon simply
no longer consumes them.

### Menu structure (amends ADR-0031's flat engine list)

`handle_menu_getlayout` builds each language as a top-level entry. A language
with at least one declared engine becomes a dbusmenu submenu parent
(`children-display=submenu`); the children are the engines that declare that
language, addressed by `TYPIO_TRAY_ENGINE_BASE + global_index` so a click
resolves back through `typio_registry_list_ordered_keyboards`. A layout-only
language (no engine) stays a flat, directly clickable item that switches the
language slot.

Engines that declare multiple registered languages appear under the first
matching language only, because dbusmenu item IDs must be unique across the
whole tree. Engines that declare none of the registered languages (for
example a legacy `language = "und"` manifest) fall through to a trailing
flat Engines section so they remain reachable.

Clicking an engine inside a language submenu switches the parent language
first (`language:<tag>`) and then the engine (`engine:<name>`), so picking
an engine commits to its language. The registry activation that follows
from `language:<tag>` would already resolve a default engine; the explicit
`engine:<name>` overrides that default when the user picked a non-default
engine for the language.

### Language label disambiguation

`typio_language_endonym` keeps returning the short endonym ("中文", "English")
for tooltips and other short surfaces. The tray menu uses a local helper,
`menu_language_label`, that appends a script qualifier when the tag carries
an ISO 15924 subtag: `zh-Hans` renders as "中文 (简)", `zh-Hant` as
"中文 (繁)", `sr-Latn` as "Srpski (Latin)". Tags with only a primary subtag
or a region subtag collapse to the bare endonym, so the common case is
unchanged.

## Alternatives considered

- **Keep ADR-0032's chain, encourage per-language config icons.** Rejected:
  the badge was the scalable answer in ADR-0032 precisely because a themed
  icon per language does not scale to BCP 47. Pushing the badge below the
  engine icon meant it almost never surfaced.
- **Disambiguate menu labels only, leave the icon alone.** Rejected: the
  duplicate "中文" was one symptom; the icon not reading as the language was
  the more visible complaint and the ADR-0032 intent.
- **Replace the flat engine list with a `languages.<tag>.keyboard` config
  verb.** Rejected: the config path is already how the default engine for a
  language is chosen (ADR-0031). The menu's job is runtime selection, which
  is a different concern; nesting engines under their language satisfies it
  without a new IPC verb.
- **Render the language badge only when the engine has no icon.** Rejected:
  this is what ADR-0032 already does and what produced the original
  complaint. The badge needs to lead unconditionally when a language is
  active.
- **Keep the dynamic mode/schema icon as a layer above the badge.** Rejected:
  mode icons and language identity compete for the same one-glance slot, and
  in practice every engine that ships a mode icon ends up suppressing the
  badge — the original symptom. Mode changes are still visible in the panel
  and tooltip; the tray base icon stays a stable language signal.
- **Keep the engine manifest icon as a fallback when no badge glyph exists.**
  Rejected: a fallback that fires only for exotic tags still leaks engine
  identity onto a language surface when the user least expects it. The
  generic keyboard icon is a clearer "active but no specific icon" glyph
  than a random engine's logo.

## Consequences

- Positive: the tray icon always shows the active language; switching
  languages visibly retargets the icon even when the underlying engine is
  the same backend configured differently per language.
- Positive: the menu's structure matches the language→engine model the
  shortcut and IPC surfaces already expose; users no longer correlate two
  flat lists.
- Positive: script variants of one primary language no longer collapse to
  indistinguishable menu entries.
- Positive: the icon is now a stable, glanceable language signal — it no
  longer churns when engines swap modes or push status icons.
- Trade-off: engines that declare multiple registered languages appear under
  only their first matching language in the menu. Users who want a different
  anchor language for such an engine can configure it explicitly via
  `languages.<tag>.keyboard`, or the engine manifest can reorder its
  `languages` array.
- Trade-off: engine identity (Rime's logo, an engine's mode icon) is no
  longer visible on the tray base icon. That identity was already redundant
  with the language badge at 22px; it remains available in the menu (future
  work: per-engine `icon-name` on submenu children), the candidate panel,
  and settings UIs.
- Trade-off: engines without language metadata, or active languages whose
  primary subtag has no badge glyph, fall through to the generic keyboard
  icon instead of an engine-supplied icon. ADR-0031's engine-cycling
  fallback still functions; the icon just no longer reflects the engine
  backend in those installations.
- Negative (accepted): a future engine that genuinely needs to surface
  per-mode state on the indicator (e.g. "voice recording now") cannot do so
  via the keyboard base icon. ADR-0032 already reserves the SNI overlay and
  attention channels for this; the keyboard base icon is the wrong channel
  for transient state anyway.
- Negative (accepted): clicking an engine in a language submenu currently
  emits two registry mutations — `language:<tag>` then `engine:<name>` —
  because libtypio exposes no atomic "set language and engine together" API.
  The language switch may resolve a different default engine first, then the
  explicit engine overrides it. The UI can briefly flicker between two
  states. The fix belongs in libtypio (`typio_registry_set_language_keyboard`
  or similar) and is tracked as future work; the host-side workaround is
  acceptable until then.

## Related

- [ADR-0031](0031-language-first-switching-surface.md) — language as the
  switch unit; the menu structure this ADR amends.
- [ADR-0032](0032-tray-icon-composition.md) — the icon precedence chain this
  ADR amends.
- [ADR-0026](0026-modality-explicit-engine-control-surface.md) — modality
  verbs that remain the substrate for within-language engine cycling.

## Follow-ups

Host-side polish applied together with this ADR; recorded here so the next
reader does not re-litigate them:

- **Native radio semantics.** The tray menu items now use dbusmenu
  `type="radio"` + `toggle-state` instead of `●` / `  ` bullet characters
  baked into the label. The host panel renders a native radio dot, and
  screen readers announce selection through the standard accessibility API
  rather than reading a stray glyph.
- **ID range partitioning.** Menu IDs are partitioned into 1000-wide
  sections (MISC=1000, LANG=2000, ENGINE=3000, VOICE=4000, PROP=5000,
  CMD=6000) so ranges never overlap regardless of how many items each
  section holds. The previous layout had `PROP` (8 × stride 32 = 256 IDs
  starting at 200) silently overlapping `LANG` and `VOICE`.
- **Single language display table.** `typio_language_endonym` and
  `typio_language_badge` share one `g_language_display` table; adding a
  language is one row, not two. The table covers ~35 widely-used languages;
  unlisted tags fall back to the raw tag for endonym and the uppercased
  primary subtag for badge.
- **Script qualifier table.** `typio_language_menu_label` looks up ISO 15924
  script codes through `g_script_display` (14 entries) instead of hard-coding
  four scripts; unlisted codes pass through verbatim so variants stay
  distinguishable.
- **Tooltip trimmed.** The active language is no longer repeated in the
  tooltip (the icon already says it); `Voice: Disabled` is omitted when no
  voice engine is configured. Common case is one line: `Keyboard: <engine>`.
- **Orphan engines labelled.** Engines that declare no registered language
  appear under an `Engines` section header rather than as mystery items.
- **Accessibility description.** Every menu item emits
  `accessible-desc` (defaulting to the label) so ATs read a clean string.
- **Push vs pull asymmetry documented.** The controller caches pushed
  engine identity but pulls the active language live from the registry
  (libtypio has no language-changed callback). `controller.c` documents
  this and warns against re-introducing a parallel icon-resolution path.
- **Pure-function test coverage.** `tests/state/test_controller_icon.c`
  covers the icon precedence chain; `tests/state/test_language_helpers.c`
  covers endonym, badge, and menu-label script disambiguation. Both run in
  CI without a `TypioInstance` fixture.
