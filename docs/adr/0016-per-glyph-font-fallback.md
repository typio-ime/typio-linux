# ADR-0016: Per-glyph font fallback with format-12 charmap selection

- **Status**: Accepted
- **Date**: 2026-05-31
- **Deciders**: Project maintainers
- **Relates to**: [ADR-0011](0011-colour-independent-coverage-glyphs.md), [ADR-0012](0012-glyph-atlas-shared-texture.md)

## Context

The panel's text shaper (`text_shaper.c`) uses HarfBuzz for shaping and FreeType
for rasterisation, with Fontconfig for font discovery. The primary font is
resolved from the `display.font_family` config key (default `"Sans"`), and the
system provides a single CJK fallback via `find_fallback_font_cached()` which
required one font to cover **all** characters in the text run.

Three distinct rendering failures were observed in the Rime schema switcher
menu:

1. **Indicator garbled text.** The Rime engine's `display_label` buffer
   (`rime_internal.h`) was 8 bytes. Schema names like "雾凇拼音" are 12 bytes
   in UTF-8. `snprintf` truncated to 7 bytes + null, splitting a 3-byte
   multi-byte sequence (`e6 8b bc` → `e6`), producing **invalid UTF-8**.
   HarfBuzz could not decode it and emitted .notdef glyphs (tofu).

2. **Schema menu tofu.** The default font `"Sans"` resolved to Noto Sans
   (Latin-only) on systems where Fontconfig did not prioritise CJK fonts.
   The whole-text fallback (`find_fallback_font_cached`) could find Noto Sans
   SC for pure CJK text, but failed on **mixed** text containing both CJK
   characters and non-CJK symbols (e.g. ballot boxes, emoji) because no single
   font covered every codepoint in the run.

3. **Emoji and symbol tofu.** Characters outside the Basic Multilingual Plane
   (U+1F480 💀, U+1F604 😄) and Miscellaneous Symbols (U+2612 ☒) appeared as
   tofu even when a covering font was installed. Root cause: FreeType selected
   the first charmap in the font file, which for Noto Emoji was a format-4
   charmap limited to BMP codepoints. HarfBuzz inherited this charmap and
   returned glyph ID 0 for supplementary-plane characters that the font
   *did* contain in its format-12 charmap.

## Decision

Three fixes, each in a different layer:

### 1. Enlarge `display_label` buffer (typio-engine-rime)

**File:** `typio-engine-rime/src/rime_internal.h:98`

`display_label[8]` → `display_label[128]`. Schema names are arbitrary-length
UTF-8 strings; the buffer must accommodate them.

### 2. Per-glyph fallback with `FT_Get_Char_Index` (typio-wayland)

**File:** `typio-wayland/src/ui/panel/text_shaper.c`

When the primary font produces `.notdef` (glyph ID 0) for a shaped glyph, the
shaper now:

1. Extracts the codepoint from the layout's owned text copy via the HarfBuzz
   cluster index.
2. Queries `FcFontSort` with a single-character `FcCharSet` to get a ranked
   list of fonts that Fontconfig claims cover that codepoint.
3. Iterates candidates, calling `FT_Get_Char_Index(face, ch)` on each to
   verify the font actually maps the codepoint to a non-zero glyph ID. This
   step is critical because Fontconfig's charset claim and FreeType's actual
   charmap lookup can disagree (see point 3 below).
4. Records the fallback glyph ID and font index per-glyph in the
   `GlyphEntry.fb_glyph_id` and `GlyphEntry.fb_idx` fields.
5. At draw time (`typio_text_shape_fill`), uses the fallback font's atlas
   entry for glyphs where `glyph_id == 0 && fb_glyph_id != 0`.

The layout struct carries up to `MAX_FALLBACK_FONTS` (4) fallback font
references, shared across all glyphs in the text run. Each `GlyphEntry` stores
a 1-byte index into this array.

**Why `FT_Get_Char_Index` instead of HarfBuzz shaping:** HarfBuzz returns
glyph ID 0 for valid codepoints when the font's selected charmap cannot
represent them (format-4 vs format-12). FreeType's `FT_Get_Char_Index` uses
the face's active charmap directly, and after selecting format-12 (see below),
it correctly resolves supplementary-plane codepoints.

### 3. Format-12 charmap selection on font load

**File:** `typio-wayland/src/ui/panel/text_shaper.c`, `get_or_create_font()`

After `FT_New_Face`, iterate `face->num_charmaps` and select the first charmap
with `FT_Get_CMap_Format() == 12` (segmented coverage, supports all Unicode).
This must happen before `hb_ft_font_create_referenced` so HarfBuzz inherits
the correct charmap.

**Requires** `#include FT_TRUETYPE_TABLES_H` for `FT_Get_CMap_Format`.

### 4. CJK-aware primary font matching

**File:** `typio-wayland/src/ui/panel/text_shaper.c`, `match_font_file()`

After `FcFontMatch`, verify the matched font covers CJK (test for U+4E2D). If
not, retry with an `FcCharSet` constraint requiring U+4E2D so Fontconfig
prefers a CJK-capable font from the same family. This prevents the common case
where `"Sans"` resolves to a Latin-only variant when a CJK variant of the same
family exists.

## Alternatives considered

- **Pango**: Provides automatic font fallback and complex text layout, but
  depends on cairo/GTK infrastructure and would replace the entire text shaping
  pipeline. Rejected: the current HarfBuzz + FreeType stack is minimal, has no
  toolkit dependency, and the per-glyph fallback adds ~2ms per unique text run
  (one-time, cached in the LRU).

- **Whole-text fallback with multiple FcFontSort passes**: Run
  `FcFontSort` once with the full charset, accept the best font even if it
  doesn't cover everything, then run again for remaining uncovered codepoints.
  Rejected: more complex than per-glyph, and the per-glyph approach naturally
  handles arbitrary mixes of CJK, symbols, and emoji without special-casing.

- **Noto Color Emoji for colour emoji**: The `CBDT`/`CBLC` tables in Noto
  Color Emoji require bitmap blitting, not R8 coverage. The panel's atlas
  architecture (R8 coverage + tint, ADR-0011/0012) cannot render colour emoji
  without a separate pipeline. Monochrome emoji from Noto Emoji or DejaVu Sans
  are rendered as tinted coverage, consistent with the rest of the panel.

## Consequences

- **Positive**: CJK text, Unicode symbols (☐ ☑ ☒ ✓ ✗), and monochrome emoji
  (💀 😄) now render correctly in mixed-script text runs. The fallback is
  per-glyph and cached in the LRU, so repeated text (candidate paging) has
  zero additional cost after the first shape.

- **Trade-off**: Each layout creation now may load up to 4 additional font
  files via `get_or_create_font`. These are cached in the font object cache
  (never freed until shutdown), so the cost is paid once per unique font file.
  The `FcFontSort` call per missing glyph is the dominant cost (~1ms per
  codepoint), but it only runs for text with missing glyphs (rare in normal
  typing; common in the Rime switcher menu which mixes CJK, symbols, and emoji).

- **Negative (accepted)**: Colour emoji (CBDT/CBLC, COLRv1, SVG tables) are
  not supported. Emoji render in monochrome via the first available monochrome
  emoji font (typically DejaVu Sans or Noto Emoji). This is consistent with the
  panel's R8-coverage rendering model.
