# Developer Setup

This document is for contributors who modify `typio-linux` source code.

## Quick Start

All commands in this document run from the `typio-linux` repository root
unless a block says otherwise.

```bash
cargo build --release --manifest-path ../libtypio/Cargo.toml
meson setup ../../flux/build ../../flux    # one-time per flux checkout
meson compile -C ../../flux/build

export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo build -p typio-host
cargo test -p typio-host
```

Run the daemon:

```bash
./target/debug/typio --verbose
```

The shipping daemon is the Rust `typio` binary from `crates/typio-host`.
The old Meson host build is no longer the contributor path. `flux` is still
a native C library, so the setup keeps one Meson build tree for the sibling
`flux` checkout until that library has a Cargo-native build.

## Prerequisites

Install these from your system package manager:

- Rust 1.85+ and Cargo
- Meson 1.0+ and Ninja 1.10+ for the sibling `flux` library
- C23 compiler and `pkg-config`
- Wayland client libraries and `xkbcommon`
- Vulkan headers and loader
- FreeType, HarfBuzz, fontconfig
- PipeWire development headers for the `voice` feature
- `glslangValidator` for the `flux` build

Versions are not capped; the project is tested against current Arch Linux
and Fedora releases.

## Repository Layout

`typio-linux` lives under a `typio/` umbrella alongside the framework
library, engine packages, and tools. `flux` is a sibling of `typio/`
because the canvas library is shared with non-Typio projects:

```text
projects/
├── typio/
│   ├── libtypio/          # cargo --manifest-path ../libtypio/Cargo.toml
│   ├── typio-engine-compose/    # Cargo: Latin + compose-key keyboard
│   ├── typio-engine-rime/       # Meson: RIME-based Chinese keyboard
│   ├── typio-engine-mozc/       # Meson: Mozc-based Japanese keyboard
│   └── typio-linux/       # run typio-linux commands here
└── flux/                  # meson setup ../../flux/build ../../flux
```

`typio-linux` has path dependencies on `../libtypio` and
`../../flux/crates/flux-sys`, so those checkouts must exist for Cargo
builds.

## Build Dependencies

Build `libtypio` first:

```bash
cargo build --release --manifest-path ../libtypio/Cargo.toml
```

This produces `libtypio.so` in `../libtypio/target/release`. Keep that
directory on `LD_LIBRARY_PATH` when running Cargo-built typio binaries and
tests.

Build `flux` next. `meson setup` is one-time per checkout; `meson compile`
rebuilds on demand:

```bash
meson setup ../../flux/build ../../flux
meson compile -C ../../flux/build
```

`flux-sys` reads `../../flux/build/meson-uninstalled/flux-uninstalled.pc`,
generates bindings from the in-tree flux headers, and links the host against
`../../flux/build/libflux.so`. If Cargo reports an undefined `flux_*` symbol,
rebuild `flux` and make sure `LD_LIBRARY_PATH` includes `../../flux/build`.

## Build typio-linux

Build the debug daemon:

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo build -p typio-host --bin typio
```

Build the release daemon:

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo build --release -p typio-host --bin typio
```

Cargo features:

| Feature | Default | When to use it |
|---|---:|---|
| `wayland` | yes | Wayland input-method frontend and flux-backed Panel |
| `systray` | yes | StatusNotifierItem tray over D-Bus via zbus |
| `voice` | yes | PipeWire capture and the `voice_input` host capability |

Disable default features only when isolating a non-Wayland Rust subsystem:

```bash
cargo test -p typio-host --no-default-features
```

## Run the Daemon

Run the debug binary:

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
./target/debug/typio --verbose
```

For engine work, point the daemon at one or more manifest directories.
`--engine-dir` is repeatable and takes highest precedence. Pass the
directory that contains `typio-engine-*.toml`; scanning is flat and does not
recurse. Manifest locations differ per engine — see the table below:

```bash
./target/debug/typio -v \
  --engine-dir ../typio-engine-compose \
  --engine-dir ../typio-engine-rime/build \
  --engine-dir ../typio-engine-mozc/build
```

Equivalently, set the colon-separated `$TYPIO_ENGINE_PATH` once:

```bash
export TYPIO_ENGINE_PATH="$PWD/../typio-engine-compose:$PWD/../typio-engine-rime/build:$PWD/../typio-engine-mozc/build"
./target/debug/typio -v
```

The daemon auto-loads only from the system engine directory. `--engine-dir`
and `$TYPIO_ENGINE_PATH` are explicit development/test opt-ins; no per-user
engine directory is scanned by default. See
[ADR-0025](../adr/0025-engine-discovery-search-path.md).

## Load a Keyboard Engine

`typio` starts without a keyboard engine, but it has nothing to convert
keystrokes with. Build an engine and pass its manifest directory to exercise
input conversion.

The keyboard engines that ship as siblings under `typio/`:

| Engine | Build system | Manifest path | Languages |
|---|---|---|---|
| `typio-engine-compose` | Cargo | `typio-engine-compose/typio-engine-compose.toml` | Latin with compose-key picker |
| `typio-engine-rime` | Meson (needs `librime`, `libcurl`) | `typio-engine-rime/build/typio-engine-rime.toml` | Chinese (zh) |
| `typio-engine-mozc` | Meson (needs Mozc depot) | `typio-engine-mozc/build/typio-engine-mozc.toml` | Japanese (ja) |

Build the Cargo engine with `cargo build --release` and the Meson engines
with `meson setup build && meson compile -C build` from their own roots,
e.g.:

```bash
cargo build --release --manifest-path ../typio-engine-compose/Cargo.toml
meson compile -C ../typio-engine-rime/build    # first time: meson setup ../typio-engine-rime/build ../typio-engine-rime
```

Then point the daemon at the directory that contains the manifest (note the
`/build` suffix for Meson engines):

```bash
./target/debug/typio -v --engine-dir ../typio-engine-compose
./target/debug/typio -v --engine-dir ../typio-engine-rime/build
```

## Run Tests

Run the Cargo suite:

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo test -p typio-host
```

Run one test:

```bash
cargo test -p typio-host service::tests::hello_reports_protocol_and_capabilities
```

See [Testing](testing.md) for test ownership rules and common Cargo test
commands.

## Install

Install the already-built Cargo binary, systemd user service, icons, and
example configs:

```bash
cargo build --release -p typio-host --bin typio
cargo xtask install --prefix /usr/local
```

Preview the install plan without writing files:

```bash
cargo xtask install --prefix /usr/local --dry-run
```

Remove installed files:

```bash
cargo xtask uninstall --prefix /usr/local
```

## Icons in Development

The system tray reports `IconName` and `IconThemePath` over D-Bus. During
development, `IconThemePath` points to `data/icons/hicolor/`, so most panels
find custom icons without installation.

If the panel ignores `IconThemePath`, install the icons into your user icon
theme:

```bash
mkdir -p ~/.local/share/icons/hicolor/scalable/apps
cp data/icons/hicolor/scalable/apps/*.svg ~/.local/share/icons/hicolor/scalable/apps/
gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
```

## See Also

- [Testing](testing.md)
- [Code Style](code-style.md)
- [Engine Discovery Reference](../reference/engine-discovery.md)
