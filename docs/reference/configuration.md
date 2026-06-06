# Configuration Reference

Typio uses two files under `$XDG_CONFIG_HOME/typio` (default `~/.config/typio`):

| File | Owner | What it controls |
|------|-------|------------------|
| `core.toml` | **libtypio** (framework) | Engine registry, shortcuts, notifications, voice runtime, per-engine settings |
| `platform.toml` | **typio-linux** (this host) | Panel styling: theme, fonts, colours, layout |

Both files are read from the same directory.  The directory itself is created
and managed by libtypio (`typio_instance_get_config_dir`).  If you need a
custom path, use the `-c` / `--config` CLI flag.

## Quick start

1. Copy the examples:
   ```bash
   mkdir -p ~/.config/typio
   cp data/core.toml.example ~/.config/typio/core.toml
   cp data/platform.toml.example ~/.config/typio/platform.toml
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

**Consumed by typio-linux.**  These keys control startup health notifications
and runtime toast behaviour.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `enable` | bool | `true` | Master switch for all notifications. |
| `startup_checks` | bool | `true` | Show a notification on startup if no engine is active or the registry is broken. |
| `runtime` | bool | `true` | Allow runtime notifications (e.g. engine errors). |
| `voice` | bool | `true` | Allow voice-specific notifications (recording started / stopped / error). |
| `cooldown_ms` | int | *(backend default)* | Minimum milliseconds between two notifications. |

### `[engines.basic]` — basic engine routing

**Consumed by typio-linux** (input router).  These keys change how the
Wayland frontend routes printable keys when the basic engine is active.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `printable_key_mode` | string | `"forward"` | `"forward"` sends printable keys through virtual-keyboard forwarding; `"commit"` lets the basic engine commit them directly. |
| `compose` | bool | `false` | Enable compose sequences (`' + a → á`, `` ` + e → è ``, etc.).  When enabled, printable keys are always routed to the engine so dead-key composition works. |

### `[engines.rime]`, `[engines.mozc]`, etc. — engine-owned sections

Each engine package registers its own `engines.<name>.*` schema.  The host does
not interpret these keys; it passes the config table to the engine via the
engine ABI.

Engines obtain their data directory via `typio_instance_get_engine_data_dir()`
(returns `<data_dir>/<engine_name>/`, e.g. `~/.local/share/typio/rime/`), so
users do not need to configure `user_data_dir` manually. Only system-level
paths (like Rime's `shared_data_dir`) remain as config keys.

Common keys you may see in examples:

```toml
[engines.rime]
shared_data_dir = "/usr/share/rime-data"
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

### Disabling engines

Installed engines can be excluded from loading entirely.  Disabled engines
are not started and never appear in the engine list.  Useful when you
have many engines installed but only want a subset active.

| Key | Type | Description |
|-----|------|-------------|
| `keyboard.disabled` | string | Comma-separated keyboard engine names to skip (e.g. `"rime, mozc"`). |
| `voice.disabled` | string | Comma-separated voice engine names to skip (e.g. `"whisper"`). |

Per-engine voice settings (illustrative):

```toml
[engines.whisper]
language = "zh"
model    = "base"      # ggml model name; place at ~/.local/share/typio/whisper/ggml-<model>.bin

[engines.sherpa-onnx]
language = "auto"
model    = "sensevoice-small"   # directory name under ~/.local/share/typio/sherpa-onnx/
```

---

## `platform.toml`

### `[display]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `panel_theme` | string | `"auto"` | `"auto"` (follow desktop), `"light"`, or `"dark"`. |
| `candidate_layout` | string | `"horizontal"` | `"horizontal"` or `"vertical"`. |
| `font_size` | int | `11` | Panel text size in points (6–72). |
| `font_family` | string | `"Sans"` | Font family name.  Use a name known to Fontconfig. |
| `panel_mode_indicator` | bool | `false` | Show the engine mode label (e.g. "中" / "A") inside the Panel. |
| `anchor_probe` | bool | `true` | Send one no-op input-method commit per activation when positioned status UI is waiting for a cursor anchor. |
| `anchor_probe_timeout_ms` | int | `150` | Milliseconds to wait for anchor readiness before dropping pending positioned status UI (50–1000). |

### `[display.colors.light]` and `[display.colors.dark]`

Custom colour overrides.  Hex strings: 6-digit (`#rrggbb`) or 8-digit
(`#rrggbbaa`) with alpha.  Omit a key to keep the built-in default for that
channel.

| Key | Description |
|-----|-------------|
| `background` | Panel background (RGBA) |
| `border` | Panel border (RGBA) |
| `text` | Candidate text colour |
| `muted` | Candidate index labels and mode indicator |
| `preedit` | Preedit text colour |
| `selection` | Selected-row highlight (RGBA) |
| `selection_text` | Text colour on the selected row |

---

## Reload behaviour

| File | Hot-reload | Notes |
|------|------------|-------|
| `platform.toml` | ✅ Yes | The Wayland frontend watches the config directory via inotify.  A `CLOSE_WRITE`, `MOVED_TO`, or attribute change **to `core.toml` or `platform.toml`** triggers a debounced reload (100 ms); editor swap/backup files and other directory churn are ignored. |
| `core.toml` | ✅ Yes | libtypio reloads the file on the same inotify event.  The frontend then re-queries shortcuts, voice engine, and notification settings. |

Keys that **require a restart** to take effect:

- `engines.*` schema changes (engine workers read their config at load time)

---

## See also

- `data/core.toml.example` — annotated starter for framework policy
- `data/platform.toml.example` — annotated starter for Panel styling
- [Engine Discovery Reference](engine-discovery.md) — how engine manifests are found and named
