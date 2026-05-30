# Wayland Frontend Configuration

The Typio Wayland frontend reads its own configuration from
`$XDG_CONFIG_HOME/typio/wayland.toml` (default
`~/.config/typio/wayland.toml`), separately from libtypio's `core.toml`.

This file owns popup styling. Framework policy, shortcuts, voice runtime,
and per-engine settings live in `core.toml`; last-used engine selection
lives in state. See libtypio's `docs/reference/configuration.md` for those.

## `[display]` section

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `panel_theme` | string | `"auto"` | `"auto"`, `"light"`, or `"dark"` |
| `candidate_layout` | string | `"vertical"` | `"horizontal"` or `"vertical"` |
| `font_size` | int | `11` | Popup text size (6–72) |
| `font_family` | string | `"Sans"` | Font family name |
| `panel_mode_indicator` | bool | `false` | Show engine mode label in popup |

## `[display.colors.light]` and `[display.colors.dark]`

Custom color overrides. Hex strings: 6-digit (`#rrggbb`) or 8-digit
(`#rrggbbaa`) with alpha. Omit a key to keep the built-in default for that
channel.

| Key | Description |
|-----|-------------|
| `background` | Popup background (RGBA) |
| `border` | Popup border (RGBA) |
| `text` | Candidate text color |
| `muted` | Candidate index labels and mode indicator |
| `preedit` | Preedit text color |
| `selection` | Selected-row highlight (RGBA) |
| `selection_text` | Text color on selected row |

## Reload

The frontend re-reads `wayland.toml` whenever the config directory's
inotify watch fires (the same mechanism that triggers libtypio's
`reload_config`). No restart required.

## See also

- `data/wayland.toml.example` — annotated starter
- libtypio's `docs/reference/configuration.md` — keys owned by `core.toml`
