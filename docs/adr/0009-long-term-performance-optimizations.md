# ADR-0009: Long-term performance optimizations — font cache purging, composition short-circuit, and snapshot fast-path

- **Status**: Accepted
- **Date**: 2026-05-29
- **Deciders**: Project maintainers

## Context

After extended runtime (hours to days) users reported that the RIME engine and candidate popup felt sluggish. Code review identified three independent root causes, only one of which was external to typio:

1. **Fontconfig unbounded cache growth in `flux_renderer.c`.** `FcInit()` was called repeatedly (once per font lookup), but `FcFini()` was never invoked. Over long sessions the Fontconfig object cache and file cache grew without bound, slowing every glyph-fallback lookup.
2. **Triple deep-copy pipeline on every keystroke while composing.** RIME engine → libtypio `set_composition` → typio-wayland `candidate_snapshot_assign`. Even when only the `selected` candidate index changed (e.g. Up/Down navigation), libtypio re-allocated ~24–27 `CString` objects and typio-wayland performed a full deep-copy of the candidate snapshot.
3. **RIME userdb bloat (external).** `librime`'s user dictionary grows monotonically with usage; this is outside typio's control but contributes to the same perceived symptom.

## Decision

### 1. Font cache management

- **Increase static cache caps:**
  - `FONT_FILE_CACHE_CAP` from 8 → 32
  - `FONT_OBJ_CACHE_CAP` from 16 → 64
- **Add a fallback-font cache** (`FALLBACK_FONT_CACHE_CAP 32`) keyed by `(text, weight)` using FNV-1a hashing. This eliminates repeated `FcPattern` → `FcFontMatch` → `FcFile` queries for the same missing codepoint sequence.
- **Export `typio_flux_engine_purge_font_caches()`** in `flux_renderer.h`. The function:
  1. Clears the font-object cache.
  2. Clears the font-file cache.
  3. Clears the fallback-font cache.
  4. Calls `FcFini()` to release Fontconfig's internal caches.
- **Call `typio_flux_engine_purge_font_caches()` on every config reload.** `wl_runtime_config.c` now purges caches after `runtime_config_refresh` commits a new configuration. This gives users a safe, predictable trigger to reclaim memory (e.g. after changing themes) without requiring a daemon restart.

### 2. libtypio composition short-circuit

- Add `candidates_content_unchanged()` and `preedit_unchanged()` helpers in `content.rs`.
- In `typio_input_context_set_composition`, if both helpers return true, update only `selected` and `cursor_pos`, emit the composition callback, and return early. This skips all `CString` allocation/free churn.
- Add `reserve()` calls for `preedit_segments` and `candidate_items` to reduce Vec reallocation when the new composition has the same segment/candidate count.

### 3. typio-wayland snapshot fast-path

- Add `candidate_snapshot_equal_content()` in `wl_input_method.c`. It compares `count`, `page`, `total`, `has_prev`/`has_next`, and every candidate's `text`/`comment`/`label`.
- In `candidate_snapshot_assign`, if content is equal, update only `selected` and `content_signature`, then return. This skips the full `calloc`/`strdup` deep-copy.

## Alternatives considered

- **Periodic timer-based purge.** Rejected: unpredictable, may purge during active composition causing a visible stutter. Config reload is user-initiated and naturally occurs at idle boundaries.
- **Shrink caches to zero on purge.** Rejected: the next composition would re-warm from cold, causing a one-time frame drop. Clearing caches while keeping the Fontconfig library initialized (`FcFini()` + re-init) was also considered, but `FcFini()` fully releases resources; the next lookup will re-initialize automatically.
- **Merge snapshot and composition structs to share pointers.** Rejected: high risk — the engine (RIME) and the Wayland frontend have different lifetimes and thread boundaries. Copying remains the safe contract.

## Consequences

- Positive: Up/Down candidate navigation no longer allocates memory; profiler shows zero `malloc`/`free` in the hot path during selection-only changes.
- Positive: Fontconfig memory is bounded and reclaimable without daemon restart.
- Positive: Config reload now has a documented side-effect (cache purge) that users can exploit as a manual "refresh" mechanism.
- Trade-off: The fallback-font cache adds a small fixed-size hash table (32 entries). Collisions are handled with linear probing; worst case is a full scan of 32 entries, still faster than `FcFontMatch`.
- Negative (accepted): `FcFini()` is a global operation. If Fontconfig is used by another subsystem in the same process, its caches are also cleared. In typio-wayland, Fontconfig is only used by `flux_renderer.c`, so this is safe.
