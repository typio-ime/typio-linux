# How to Package for Distribution

Build and install `typio-wayland` for system-wide or package-manager distribution.

## Build a release binary

```bash
meson setup build --buildtype=release \
    -Denable_systray=true \
    -Denable_status_bus=true \
    -Denable_voice=true
ninja -C build
```

Disable features your target environment does not support:

| Option | When to omit |
|--------|--------------|
| `-Denable_systray=true` | Target desktop has no StatusNotifierItem support |
| `-Denable_voice=true` | Target does not ship PipeWire or voice engines |

## Install into a staging directory

Use `DESTDIR` to stage files for a package manager without touching the live system:

```bash
DESTDIR=/tmp/typio-staging meson install -C build
```

Meson respects the `prefix` chosen at setup time (default `/usr/local`).
Override it if your distribution uses a different prefix:

```bash
meson setup build --prefix=/usr --buildtype=release
```

## What gets installed

| File | Destination | Purpose |
|------|-------------|---------|
| `typio` | `<prefix>/<bindir>/` | Main daemon binary |
| `typio.service` | `<prefix>/<libdir>/systemd/user/` | systemd user service unit that runs the daemon |
| `hicolor/*` | `<datadir>/icons/` | Status and tray icons |
| `core.toml.example` | `<datadir>/typio/` | Example core configuration |
| `wayland.toml.example` | `<datadir>/typio/` | Example Wayland frontend configuration |

## Runtime dependencies

The binary itself requires:

- `libtypio` — the framework library (resolved at link time)
- `wayland-client`, `xkbcommon` — for the Wayland frontend
- `dbus-1` — for status bus and tray (if enabled at build time)
- `libpipewire-0.3` — for voice capture (if enabled at build time)
- Vulkan loader, FreeType, HarfBuzz, fontconfig — for the candidate Panel renderer

## Engine plugins

`typio` does not ship with input engines. At minimum, install one engine plugin
into `<libdir>/typio/engines/`. The file must match `libtypio_engine_*.so`.

Common engines:

- `libtypio_engine_basic.so` — zero-dependency fallback keyboard
- `libtypio_engine_rime.so` — RIME-based engine
- `libtypio_engine_whisper.so` — Whisper voice engine

Package each engine as a separate package so users choose only the ones they need.

See the [Engine Discovery Reference](../reference/engine-discovery.md) for the
search-path order, file-name rules, and bundled-icon layout. Per-user or
development engine directories are explicit runtime overrides, not packaged
defaults.

## Configuration

Copy the example files to the system or user config directory and edit them:

```bash
mkdir -p /etc/typio
cp <datadir>/typio/core.toml.example /etc/typio/core.toml
cp <datadir>/typio/wayland.toml.example /etc/typio/wayland.toml
```

Or per-user:

```bash
mkdir -p ~/.config/typio
cp <datadir>/typio/core.toml.example ~/.config/typio/core.toml
cp <datadir>/typio/wayland.toml.example ~/.config/typio/wayland.toml
```

See [Configuration Reference](../reference/configuration.md) for key meanings.

## systemd service

The installed user unit (`typio.service`) starts the daemon as part of the
graphical session. Enable it per-user:

```bash
systemctl --user enable typio.service
systemctl --user start typio.service
```

Follow daemon logs through the user journal:

```bash
journalctl --user -u typio -f
```

Do not package a desktop entry that executes `typio` directly. The daemon is
session infrastructure: direct desktop launch loses service supervision, restart
policy, duplicate-start protection, and a stable journal unit for logs. For the
decision background see [ADR-0021](../adr/0021-systemd-user-service-daemon-lifecycle.md).

## Packaging checklist

- [ ] `libtypio` is available in the target repository or bundled via the meson wrap
- [ ] At least one engine plugin is packaged or declared as a dependency
- [ ] `DESTDIR` staging produces clean file lists
- [ ] The systemd user unit path matches the distribution's `<libdir>`
- [ ] Icon cache update is triggered after installation if required by the distribution policy
