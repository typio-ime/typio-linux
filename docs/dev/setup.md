# Developer Setup

This document is for contributors who will modify `typio-wayland` source code.

## Requirements

- Meson 1.0+ (primary build system)
- Ninja 1.10+
- C23 compiler
- `pkg-config`
- Wayland client development files
- `xkbcommon` development files
- `wayland-scanner`
- Vulkan, FreeType, HarfBuzz, and fontconfig development files
- `glslangValidator`
- [libtypio](https://github.com/ming2k/libtypio) — installed system-wide, or resolved via the meson wrap (see below)

Optional:

- `dbus-1` for `enable_status_bus=true` or `enable_systray=true`
- `libpipewire-0.3` for `-Dbuild_voice=true`

Engines (rime, mozc, …) are separate projects; their dependencies are documented in those repositories.

## External dependencies

| Dependency | Source | Resolved version | You need to install it? |
|---|---|---|---|
| **libtypio** | pkg-config (preferred), or `subprojects/libtypio.wrap` fallback | matching version | **No** — system install or auto-fetched by the wrap |
| **flux** (rendering framework) | `subprojects/flux.wrap` (git fallback) or sibling checkout | latest | **No** — resolved automatically |

libtypio is resolved by pkg-config first.  Point `PKG_CONFIG_PATH` at a
local `cargo build` (`/path/to/libtypio/target/release`) for active
development, or rely on a system install.  If pkg-config can't find it,
Meson falls back to `subprojects/libtypio.wrap`, which clones libtypio and
builds it via cargo.

flux is resolved as a Meson subproject when `subprojects/flux.wrap` (or a local `subprojects/flux/` checkout) is present. If the subproject is unavailable, the build continues with candidate popup rendering disabled (stubs are used).

System libraries are discovered via `pkg-config`. Meson does not enforce upper-bound versions, but the project is regularly tested against the packages shipped in the latest Arch Linux and Fedora releases.

## Local development workflow

For active work on libtypio you'll want a local checkout next to
typio-wayland and discover it via `pkg-config`.  When you're not
touching libtypio internals, the wrap path (`subprojects/libtypio.wrap`)
or a system install is enough.

| Checkout | Purpose | Build system |
|---|---|---|
| `libtypio` (anywhere on disk) | Core framework library (C ABI) | `cargo` |
| `typio-engine-basic` (anywhere) | Fallback keyboard engine plugin | `cargo` |
| `flux` | Candidate popup renderer | Meson subproject (auto) |

### 1. Build libtypio

```bash
cd /path/to/libtypio
cargo build --release
```

This produces `target/release/libtypio.so` and the public C headers under
`include/typio/`.  It also generates `libtypio.pc` and
`typio-engine-abi.pc` directly in `target/release/` so C consumers can
discover the library via `pkg-config`.

You do **not** need to install these files system-wide for local
development. The rest of this guide points `PKG_CONFIG_PATH` and
`LD_LIBRARY_PATH` directly at `target/release/`. System installation
(like `make install`) is only needed when packaging for distribution.

### 2. Build typio-engine-basic

`typio` needs at least one keyboard engine plugin to function.
The `typio-engine-basic` repository provides the zero-dependency fallback.

```bash
cd ../typio-engine-basic
cargo build --release
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

### 3. Build typio-wayland

Point `PKG_CONFIG_PATH` at libtypio's `target/release` (where the `.pc`
files were generated) before running Meson.  If the variable is not set
and the package is not installed system-wide, Meson falls back to the
`subprojects/libtypio.wrap` and clones+builds libtypio automatically.

```bash
cd typio-wayland
export PKG_CONFIG_PATH="/path/to/libtypio/target/release:${PKG_CONFIG_PATH}"
meson setup build --buildtype=debug -Denable_systray=true
ninja -C build
```

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

At startup `typio` searches for engine plugins in the following order:

1. `TYPIO_ENGINE_DIR` environment variable (if set)
2. `--engine-dir <path>` command-line flag (if given)
3. `~/.local/share/typio/engines`
4. The compile-time `TYPIO_ENGINE_DIR` (usually `/usr/local/lib/typio/engines`)

In each directory it looks for files matching `libtypio-engine-*.so`,
`dlopen`s each one, and registers it with libtypio via the engine ABI.
The file name prefix is mandatory; the suffix becomes the engine name
exposed to users (`basic`, `rime`, `whisper`, …).

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

## Run the daemon while iterating

The built binary links against `../libtypio/target/release/libtypio.so`,
so you must add that directory to the dynamic linker path at runtime:

```bash
export LD_LIBRARY_PATH=/absolute/path/to/libtypio/target/release:${LD_LIBRARY_PATH}
./build/src/typio --list
./build/src/typio --verbose
```

For plugin engine work, point the daemon at a custom engine directory:

```bash
export TYPIO_ENGINE_DIR=/absolute/path/to/engines
./build/src/typio --engine-dir /absolute/path/to/engines --engine rime --verbose
```

Both `TYPIO_ENGINE_DIR` and `--engine-dir` accept a single path. If you
need to test multiple engine directories at once, symlink them into a
single directory and point the daemon there.

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
