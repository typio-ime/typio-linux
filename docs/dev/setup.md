# Developer Setup

This document is for contributors who will modify `typio-wayland` source code.

## Quick start

```bash
export PKG_CONFIG_PATH="../libtypio/target/release:${PKG_CONFIG_PATH}"
meson setup build --buildtype=debug -Denable_systray=true
ninja -C build
```

If `libtypio` is not built locally, Meson clones and builds it automatically
via `subprojects/libtypio.wrap`.  `flux` is handled similarly (see below).

Run:

```bash
export LD_LIBRARY_PATH=../libtypio/target/release:${LD_LIBRARY_PATH}
./build/src/typio --verbose
```

## Prerequisites

Install these from your system package manager:

- Meson 1.0+, Ninja 1.10+
- C23 compiler, `pkg-config`
- `wayland-scanner`, `glslangValidator`
- Wayland client libraries, `xkbcommon`
- Vulkan, FreeType, HarfBuzz, fontconfig
- `dbus-1` (if `-Denable_systray=true`)
- `libpipewire-0.3` (if `-Dbuild_voice=true`)

Versions are not capped; the project is tested against latest Arch Linux and
Fedora releases.

## Project dependencies

Two repositories are resolved automatically.  You do **not** need to install
them system-wide.

| Dependency | Resolution | Notes |
|---|---|---|
| **libtypio** | `pkg-config` first, else subproject wrap | Point `PKG_CONFIG_PATH` to a local `../libtypio/target/release` build, or let Meson clone and build it via cargo.  Runtime requires `LD_LIBRARY_PATH` to find the shared library. |
| **flux** | sibling `../flux` or subproject wrap | Always built as **static** and linked into the binary.  If unresolved, candidate-popup rendering is disabled (stubs are used). |

**Recommended layout** for active development:

```
parent/
├── libtypio/          ← cargo build --release
├── flux/              ← optional; auto-symlinked as subprojects/flux
└── typio-wayland/     ← run all commands from here
```

**Stale `subprojects/flux` symlink?**  If a sibling `../flux` checkout was
moved or deleted, Meson may find a dangling symlink and refuse to fall back
to the wrap.  Fix it:

```bash
rm -f subprojects/flux
meson setup --reconfigure build
```

## Local development workflow

For active work on libtypio you'll want a local checkout next to
typio-wayland and discover it via `pkg-config`.  When you're not
touching libtypio internals, the wrap path (`subprojects/libtypio.wrap`)
or a system install is enough.

| Checkout | Purpose | Build system |
|---|---|---|
| `libtypio` | Core framework library (C ABI) | `cargo` |
| `typio-engine-basic` | Fallback keyboard engine plugin | `cargo` |
| `flux` | Candidate popup renderer | Meson subproject (auto) |

The commands below assume **sibling checkouts** and that you run every
command from the `typio-wayland` directory:

```
parent/
├── libtypio/
├── typio-engine-basic/
└── typio-wayland/   ← run all commands from here
```

If your layout differs, adjust the `../libtypio` paths accordingly.

### 1. Build libtypio

```bash
( cd ../libtypio && cargo build --release )
```

This produces `target/release/libtypio.so` and the public C headers under
`include/typio/`.  It also generates `libtypio.pc` and
`typio-engine-abi.pc` directly in `target/release/` so C consumers can
discover the library via `pkg-config`.

You do **not** need to install these files system-wide for local
development. The rest of this guide points `PKG_CONFIG_PATH` and
`LD_LIBRARY_PATH` directly at `../libtypio/target/release`. System
installation (like `make install`) is only needed when packaging for
distribution.

### 2. Build typio-wayland

Point `PKG_CONFIG_PATH` at libtypio's `target/release` (where the `.pc`
files were generated) before running Meson.  If the variable is not set
and the package is not installed system-wide, Meson falls back to the
`subprojects/libtypio.wrap` and clones+builds libtypio automatically.

```bash
export PKG_CONFIG_PATH="../libtypio/target/release:${PKG_CONFIG_PATH}"
meson setup build --buildtype=debug -Denable_systray=true
ninja -C build
```

### 3. Run the daemon while iterating

The built binary links against libtypio's `target/release/libtypio.so`, so
you must add that **same** directory to the dynamic linker path at runtime:

```bash
export LD_LIBRARY_PATH=../libtypio/target/release:${LD_LIBRARY_PATH}
./build/src/typio --list
./build/src/typio --verbose
```

For plugin engine work, point the daemon at a specific engine directory
(overriding the default scan list). Once you've installed an engine (see
below), this runs it directly:

```bash
export TYPIO_ENGINE_DIR=~/.local/share/typio/engines
./build/src/typio --engine-dir ~/.local/share/typio/engines --engine basic --verbose
```

Both `TYPIO_ENGINE_DIR` and `--engine-dir` accept a single path. If you
need to test multiple engine directories at once, symlink them into a
single directory and point the daemon there.

## Install a keyboard engine (optional)

`typio` starts and runs without any keyboard engine — it just has nothing
to convert keystrokes with, so it logs a "No keyboard engine is active"
startup warning and falls back to a "No engine loaded" placeholder. To
actually exercise input conversion (and to verify your build wires up the
engine ABI correctly), install at least one engine.

Any engine works; this uses `typio-engine-basic`, the zero-dependency
fallback, as the concrete example:

```bash
( cd ../typio-engine-basic && cargo build --release )
```

This produces `../typio-engine-basic/target/release/libtypio_engine_basic.so`.

**File name convention:** Cargo uses the crate name (`typio_engine_basic`,
with an underscore) as the library file name, but `typio` scans for files
matching `libtypio-engine-*.so` (with a hyphen). You must rename the file
when installing:

```bash
mkdir -p ~/.local/share/typio/engines
cp ../typio-engine-basic/target/release/libtypio_engine_basic.so \
   ~/.local/share/typio/engines/libtypio-engine-basic.so
```

The file name suffix (`basic`) becomes the engine identifier exposed to
users and configuration files.

## Optional features

| Option | Default | When you need it |
|---|---|---|
| `-Denable_systray=true` | `false` | System tray icon |
| `-Dbuild_voice=true` | `false` | PipeWire audio capture and voice session infrastructure |

`-Dbuild_voice=true` does **not** compile any voice engine into the binary.
Voice engines (Whisper, Sherpa-ONNX, …) are separate plugin repositories
loaded at runtime. This option only enables the host-side PipeWire capture
and voice-session plumbing that those external engines plug into.

## Engine discovery

At startup `typio` scans a fixed, ordered list of directories and registers
the **first** engine of each name it finds — so earlier directories win: a
user-installed engine shadows a system one of the same name.

| Order | Source | Path |
|---|---|---|
| 1 | `-E` / `--engine-dir DIR` | directory passed on the command line |
| 2 | `$TYPIO_ENGINE_DIR` | value of that environment variable |
| 3 | **User** | `$XDG_DATA_HOME/typio/engines`, else `~/.local/share/typio/engines` |
| 4 | **System** | compile-time `<prefix>/<libdir>/typio/engines` (typically `/usr/lib/typio/engines`) |

In each directory it loads only files named `libtypio-engine-<name>.so`,
`dlopen`s each, and registers it with libtypio via the engine ABI. The
`libtypio-engine-` prefix is mandatory; `<name>` (the part before `.so`)
becomes the engine identifier exposed in config and the CLI (`basic`, `rime`,
`whisper`, …). A sibling `icons/` directory (`<engine-dir>/icons/`,
freedesktop hicolor layout) is added to the tray's icon search path, so an
engine can ship its own symbolic icons.

Canonical rules (precedence, naming, icons) live in the
[Engine Discovery Reference](../reference/engine-discovery.md).

## Run tests

```bash
meson test -C build --print-errorlogs
```

For isolated D-Bus runs (sanitizer and CI-like):

```bash
dbus-run-session -- meson test -C build --print-errorlogs
```

## Icons in development

The system tray reports `IconName` and `IconThemePath` via D-Bus. During
development `IconThemePath` automatically points to the source tree
(`data/icons/hicolor/`), so most panels (GNOME, KDE, Waybar, …) will find
custom icons without installation.

If your panel does **not** respect `IconThemePath` and shows a missing-icon
placeholder, install the icons into your user icon theme:

```bash
mkdir -p ~/.local/share/icons/hicolor/scalable/apps
cp data/icons/hicolor/scalable/apps/*.svg ~/.local/share/icons/hicolor/scalable/apps/
gtk-update-icon-cache ~/.local/share/icons/hicolor 2>/dev/null || true
```

For system-wide installation use `meson install` (requires `sudo` if your
prefix is `/usr` or `/usr/local`):

```bash
meson install -C build
```

## Meson options

| Option | Default | Meaning |
|--------|---------|---------|
| `build_tests` | `true` | Build unit and integration tests |
| `enable_wayland` | `true` | Enable the Wayland frontend |
| `enable_status_bus` | `true` | Enable the D-Bus runtime status/control interface |
| `enable_systray` | `false` | Enable StatusNotifierItem support |
| `build_voice` | `false` | Enable PipeWire audio capture and voice session infrastructure |
| `enable_asan` | `false` | Enable AddressSanitizer |
| `enable_ubsan` | `false` | Enable UndefinedBehaviorSanitizer |

## Project layout

See [project-layout.md](project-layout.md) for a tour of the source tree.

## Submitting changes

See the [Pull Request Checklist](../../CONTRIBUTING.md#pull-request-checklist) in `CONTRIBUTING.md`.
