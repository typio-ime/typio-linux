# Configuration Reference

Typio uses two files under `$XDG_CONFIG_HOME/typio` (default `~/.config/typio`):

| File | Owner | What it controls |
|------|-------|------------------|
| `core.toml` | **libtypio** (framework) | Engine registry, shortcuts, notifications, voice runtime, per-engine settings |
| `wayland.toml` | **typio-wayland** (this host) | Popup styling: theme, fonts, colours, layout |

Both files are read from the same directory.  The directory itself is created
and managed by libtypio (`typio_instance_get_config_dir`).  If you need a
custom path, use the `-c` / `--config` CLI flag.

## Quick start

1. Copy the examples:
   ```bash
   mkdir -p ~/.config/typio
   cp data/core.toml.example ~/.config/typio/core.toml
   cp data/wayland.toml.example ~/.config/typio/wayland.toml
   ```
2. Edit the keys you need.
3. Most changes are picked up **without restart** via inotify reload (see
   [Reload behaviour](#reload-behaviour) below).

---

## `core.toml`

### `[keyboard]` — framework policy

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `per_app_preferences` | bool | `true` | Remember and restore the keyboard engine and engine mode per application. |

### `[shortcuts]` — global shortcuts

Parsed by libtypio.  All values are shortcut strings such as `Ctrl+Shift` or
`Super+v`.

| Key | Default | Description |
|-----|---------|-------------|
| `switch_keyboard_engine` | `Ctrl+Shift` | Cycle to the next keyboard engine. |
| `exit` | `Ctrl+Shift+Escape` | Emergency shutdown of the daemon. |
| `voice_ptt` | `Super+v` | Push-to-talk for voice input. |

### `[notifications]` — desktop notification policy

**Consumed by typio-wayland.**  These keys control startup health notifications
and runtime toast behaviour.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enable` | bool | `true` | Master switch for all notifications. |
| `startup_checks` | bool | `true` | Show a notification on startup if no engine is active or the registry is broken. |
| `runtime` | bool | `true` | Allow runtime notifications (e.g. engine errors). |
| `voice` | bool | `true` | Allow voice-specific notifications (recording started / stopped / error). |
| `cooldown_ms` | int | *(backend default)* | Minimum milliseconds between two notifications. |

### `[engines.basic]` — basic engine routing

**Consumed by typio-wayland** (input router).  These keys change how the
Wayland frontend routes printable keys when the basic engine is active.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `printable_key_mode` | string | `"forward"` | `"forward"` sends printable keys through virtual-keyboard forwarding; `"commit"` lets the basic engine commit them directly. |
| `compose` | bool | `false` | Enable compose sequences (`' + a → á`, `` ` + e → è ``, etc.).  When enabled, printable keys are always routed to the engine so dead-key composition works. |

### `[engines.rime]`, `[engines.mozc]`, etc. — engine-owned sections

Each engine plugin registers its own `engines.<name>.*` schema.  The host does
not interpret these keys; it passes the config table to the engine via the
engine ABI.

Common keys you may see in examples:

```toml
[engines.rime]
shared_data_dir = "/usr/share/rime-data"
user_data_dir   = "~/.local/share/typio/rime"
full_check      = false

[engines.mozc]
server_path = "/usr/lib/mozc/mozc_server"
```

For authoritative schema and defaults, consult the individual engine repository.

### Engine selection (keyboard and voice)

Both keyboard and voice engines follow the same activation priority:

1. **Config override** — `keyboard.engine` / `voice.engine` in `core.toml`
2. **State persistence** — last-used engine resumed from `engine-state.toml`
3. **Auto-detection** — first registered engine of the matching type

| Key | Type | Description |
|-----|------|-------------|
| `keyboard.engine` | string | Override keyboard engine to activate on startup (e.g. `"rime"`, `"basic"`). When omitted, the last-used engine is resumed. |
| `voice.engine` | string | Override voice engine to activate on startup (e.g. `"whisper"`, `"sherpa-onnx"`). When omitted or the named engine is not installed, the first available voice engine is auto-selected. |

Per-engine voice settings (illustrative):

```toml
[engines.whisper]
language = "zh"
model    = "base"      # ggml model name; place at ~/.local/share/typio/whisper/ggml-<model>.bin

[engines.sherpa-onnx]
language = "auto"
model    = "sensevoice-small"   # directory name under ~/.local/share/typio/sherpa-onnx/
```

Voice session timeout:

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `voice.unload_after_ms` | int | `480000` | Idle milliseconds before the voice session is torn down.  Set to `0` to disable auto-unload. |

---

## `wayland.toml`

### `[display]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `panel_theme` | string | `"auto"` | `"auto"` (follow desktop), `"light"`, or `"dark"`. |
| `candidate_layout` | string | `"vertical"` | `"horizontal"` or `"vertical"`. |
| `font_size` | int | `11` | Popup text size in points (6–72). |
| `font_family` | string | `"Sans"` | Font family name.  Use a name known to Fontconfig. |
| `panel_mode_indicator` | bool | `false` | Show the engine mode label (e.g. "中" / "A") inside the popup. |

### `[display.colors.light]` and `[display.colors.dark]`

Custom colour overrides.  Hex strings: 6-digit (`#rrggbb`) or 8-digit
(`#rrggbbaa`) with alpha.  Omit a key to keep the built-in default for that
channel.

| Key | Description |
|-----|-------------|
| `background` | Popup background (RGBA) |
| `border` | Popup border (RGBA) |
| `text` | Candidate text colour |
| `muted` | Candidate index labels and mode indicator |
| `preedit` | Preedit text colour |
| `selection` | Selected-row highlight (RGBA) |
| `selection_text` | Text colour on the selected row |

---

## Reload behaviour

| File | Hot-reload | Notes |
|------|------------|-------|
| `wayland.toml` | ✅ Yes | The Wayland frontend watches the config directory via inotify.  Any `CLOSE_WRITE`, `MOVED_TO`, or attribute change triggers a debounced reload (100 ms). |
| `core.toml` | ✅ Yes | libtypio reloads the file on the same inotify event.  The frontend then re-queries shortcuts, voice engine, and notification settings. |

Keys that **require a restart** to take effect:

- `engines.*` schema changes (engine plugins read their config at load time)
- `voice.unload_after_ms` (read once when the voice session is created)

---

## See also

- `data/core.toml.example` — annotated starter for framework policy
- `data/wayland.toml.example` — annotated starter for popup styling
- [Engine Discovery Reference](engine-discovery.md) — how engine plugins are found and named
