# How to Package for Distribution

Build and install `typio-linux` for system-wide or package-manager
distribution.

## Build a Release Binary

Build the sibling native dependencies first. These commands run from the
`typio-linux` repository root:

```bash
cargo build --release --manifest-path ../libtypio/Cargo.toml
meson compile -C ../../flux/build    # first time: meson setup ../../flux/build ../../flux
```

Build the host daemon:

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo build --release -p typio-host --bin typio
```

The output binary is `target/release/typio`.

## Stage Package Files

Use `cargo xtask install` with `--destdir` to stage files for a package
manager without touching the live system:

```bash
cargo xtask install --prefix /usr --destdir /tmp/typio-staging
```

Preview the file list:

```bash
cargo xtask install --prefix /usr --destdir /tmp/typio-staging --dry-run
```

`--prefix` is the runtime install prefix recorded in paths such as the
systemd unit. `--destdir` is only the staging root.

## Installed Files

| File | Destination | Purpose |
|------|-------------|---------|
| `typio` | `<prefix>/bin/` | Main daemon binary |
| `typio.service` | `<prefix>/lib/systemd/user/` | systemd user service unit |
| `hicolor/*` | `<prefix>/share/icons/` | Status and tray icons |
| `core.toml.example` | `<prefix>/share/typio/` | Example core configuration |
| `platform.toml.example` | `<prefix>/share/typio/` | Example Wayland frontend configuration |
| `typio-engine-*.toml` | `<prefix>/share/typio/engines/` | Engine manifests installed by engine packages |

Engine manifests are listed for package layout completeness; `typio-linux`
does not install engine packages.

## Runtime Dependencies

The daemon requires:

- `libtypio`
- `wayland-client` and `xkbcommon`
- D-Bus runtime support for StatusNotifierItem tray integration
- `libpipewire-0.3` for voice capture when voice support is used
- Vulkan loader, FreeType, HarfBuzz, and fontconfig for the candidate Panel
- `libflux` from the packaged `flux` build

## Engine Packages

`typio` does not ship with input engines. At minimum, install one engine
manifest into `<prefix>/share/typio/engines/`. The file must match
`typio-engine-*.toml`.

Engine workers are private helper executables. Install them under a package
owned libexec path and point each manifest's `command` at the installed
worker path.

Common engines:

- `typio-engine-compose.toml`: zero-dependency Latin keyboard with
  compose-key picker (Cargo build)
- `typio-engine-rime.toml`: RIME-based Chinese keyboard (Meson build)
- `typio-engine-mozc.toml`: Mozc-based Japanese keyboard (Meson build)
- `typio-engine-sherpa.toml`: Sherpa-ONNX voice engine

Package each engine separately so users choose only the ones they need. See
the [Engine Discovery Reference](../reference/engine-discovery.md) for
search-path order, file-name rules, and bundled-icon layout.

## Configuration

Copy the example files to the system or user config directory and edit them:

```bash
mkdir -p /etc/typio
cp <prefix>/share/typio/core.toml.example /etc/typio/core.toml
cp <prefix>/share/typio/platform.toml.example /etc/typio/platform.toml
```

Or per-user:

```bash
mkdir -p ~/.config/typio
cp <prefix>/share/typio/core.toml.example ~/.config/typio/core.toml
cp <prefix>/share/typio/platform.toml.example ~/.config/typio/platform.toml
```

See the [Configuration Reference](../reference/configuration.md) for key
meanings.

## Systemd Service

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
session infrastructure: direct desktop launch loses service supervision,
restart policy, duplicate-start protection, and a stable journal unit for
logs. For the decision background, see
[ADR-0021](../adr/0021-systemd-user-service-daemon-lifecycle.md).

## Packaging Checklist

- [ ] `libtypio` is packaged or declared as a dependency.
- [ ] `flux` / `libflux` is packaged or declared as a dependency.
- [ ] At least one engine package is packaged or declared as a dependency.
- [ ] `cargo xtask install --destdir` produces a clean file list.
- [ ] The systemd user unit path matches the distribution's `<libdir>`.
- [ ] Icon cache update is triggered after installation if required by the distribution policy.
