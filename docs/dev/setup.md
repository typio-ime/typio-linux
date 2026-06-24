# Developer Setup

This document is for contributors who modify `typio-linux` source code.

## Quick Start

All commands in this document run from the `typio-linux` repository root
unless a block says otherwise.

```bash
# one-time per flux checkout:
meson setup ../../optics/flux/build ../../optics/flux
meson compile -C ../../optics/flux/build

# point flux-sys at the freshly built libflux (every shell that runs
# cargo build/test/run, or drop the two exports into ~/.bashrc / a local
# .envrc — the repo ships no committed copy):
export FLUX_BUILD_DIR="$PWD/../../optics/flux/build"
export FLUX_SOURCE_DIR="$PWD/../../optics/flux"

cargo build -p typio-host
cargo test -p typio-host
./target/debug/typio --verbose
```

`typio-linux` links `libflux` straight out of the `optics/flux` Meson build
tree — there is no need to install flux system-wide, and no `LD_LIBRARY_PATH`
is required (flux-sys bakes an `-Wl,-rpath` to the build tree). `libtypio`
and the `flux-sys` / `flux-text-sys` bindings resolve as git crates, so a
build needs no sibling Rust checkouts either.

The shipping daemon is the Rust `typio` binary from `crates/typio-host`.

## Prerequisites

Install these from your system package manager:

- Rust 1.85+ and Cargo
- Meson 1.0+, Ninja 1.10+, and `glslangValidator` to build `flux` from source
- C23 compiler and `pkg-config`
- Wayland client libraries and `xkbcommon`
- Vulkan headers and loader
- FreeType, HarfBuzz, fontconfig
- PipeWire development headers for the `voice` feature

Versions are not capped; the project is tested against current Arch Linux
and Fedora releases.

## Repository Layout

`typio-linux` lives under a `typio/` umbrella alongside the framework
library, engine packages, and tools. The GPU/media libraries live under a
separate `optics/` umbrella because the canvas stack is shared with
non-Typio projects:

```text
projects/
├── typio/
│   ├── libtypio/                 # published as git crate v0.5.0; local checkout optional
│   ├── typio-engine-compose/     # Cargo: Latin + compose-key keyboard
│   ├── typio-engine-rime/        # Meson: RIME-based Chinese keyboard
│   ├── typio-engine-mozc/        # Meson: Mozc-based Japanese keyboard
│   └── typio-linux/              # run typio-linux commands here
└── optics/
    ├── flux/                     # C canvas library; build in-tree and point FLUX_BUILD_DIR at it
    ├── flux-rs/                  # Rust bindings; published as git crate v0.1.0
    ├── iris/, lens/, …           # other optics components, not consumed by typio-linux
```

`typio-linux` resolves `libtypio` and `flux-sys` / `flux-text-sys` as git
crates (see the `[workspace.dependencies]` table in the root `Cargo.toml`),
so no sibling checkout is required for a build. The only external native
dependency is `libflux`, which `flux-sys`'s build script locates through
pkg-config at build time.

## flux (C library)

`flux-sys` does not vendor the C source; its build script locates `libflux`
through pkg-config. The contributor workflow builds flux in its own tree and
points the build script at it, so no system install is needed.

**In-tree build (default).** Build flux once, then point `flux-sys` at the
build tree. `meson setup` is one-time per checkout; `meson compile` rebuilds
on demand:

```bash
meson setup ../../optics/flux/build ../../optics/flux
meson compile -C ../../optics/flux/build

export FLUX_BUILD_DIR="$PWD/../../optics/flux/build"
export FLUX_SOURCE_DIR="$PWD/../../optics/flux"   # optional: bindgen from this checkout
```

`flux-sys` prepends the build tree's `meson-uninstalled/` to
`PKG_CONFIG_PATH` and bakes an `-Wl,-rpath` for it, so binaries find
`libflux.so` at runtime with no `LD_LIBRARY_PATH` and no `meson install`.
Keep the two exports set in any shell that runs `cargo build` / `test` /
the daemon; `FLUX_BUILD_DIR` is what selects the in-tree library. If Cargo
reports an undefined `flux_*` symbol, rebuild flux (`meson compile -C
../../optics/flux/build`) and re-run.

**Installed (optional).** If you prefer a system-wide flux, `meson install`
into a prefix on `PKG_CONFIG_PATH` and unset `FLUX_BUILD_DIR` (or set
`FLUX_USE_INSTALLED=1`) so pkg-config resolves the installed `flux.pc`
instead of the build tree.

## Build typio-linux

Build the debug daemon:

```bash
cargo build -p typio-host --bin typio
```

Build the release daemon:

```bash
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
