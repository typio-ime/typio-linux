# Developer Setup

This document is for contributors who will modify `typio-linux` source code.

## Quick start

```bash
export PKG_CONFIG_PATH="../libtypio/target/release:${PKG_CONFIG_PATH}"
meson setup build --buildtype=debug -Denable_systray=true -Denable_voice=true
ninja -C build
```

If `libtypio` is not built locally, Meson clones and builds it automatically
via `subprojects/libtypio.wrap`.  `flux` is handled similarly (see below).

Run:

```bash
export LD_LIBRARY_PATH=../libtypio/target/release:${LD_LIBRARY_PATH}
./build/src/typio --verbose
```

`-Denable_voice=true` enables PipeWire audio capture and the `voice_input`
host capability so voice engine plugins (e.g. Sherpa-ONNX) can load.
Without it, voice engines are rejected at startup.

## Prerequisites

Install these from your system package manager:

- Meson 1.0+, Ninja 1.10+
- C23 compiler, `pkg-config`
- `wayland-scanner`, `glslangValidator`
- Wayland client libraries, `xkbcommon`
- Vulkan, FreeType, HarfBuzz, fontconfig
- `libsystemd` (sd-bus, required when `-Denable_systray=true`)
- `libpipewire-0.3` (if `-Denable_voice=true`)

Versions are not capped; the project is tested against latest Arch Linux and
Fedora releases.

## Project dependencies

Two repositories are resolved automatically.  You do **not** need to install
them system-wide.

| Dependency | Resolution | Notes |
|---|---|---|
| **libtypio** | `pkg-config` first, else subproject wrap | Point `PKG_CONFIG_PATH` to a local `../libtypio/target/release` build, or let Meson clone and build it via cargo.  Runtime requires `LD_LIBRARY_PATH` to find the shared library. |
| **flux** | sibling `../flux` or subproject wrap | Always built as **static** and linked into the binary.  If unresolved, candidate Panel rendering is disabled (stubs are used). |

**Recommended layout** for active development:

```
parent/
├── libtypio/          ← cargo build --release
├── flux/              ← optional; auto-symlinked as subprojects/flux
└── typio-linux/     ← run all commands from here
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
typio-linux and discover it via `pkg-config`.  When you're not
touching libtypio internals, the wrap path (`subprojects/libtypio.wrap`)
or a system install is enough.

| Checkout | Purpose | Build system |
|---|---|---|
| `libtypio` | Core framework library (C ABI) | `cargo` |
| `typio-engine-basic` | Fallback keyboard engine plugin | `cargo` |
| `flux` | Candidate Panel renderer | Meson subproject (auto) |

The commands below assume **sibling checkouts** and that you run every
command from the `typio-linux` directory:

```
parent/
├── libtypio/
├── typio-engine-basic/
└── typio-linux/   ← run all commands from here
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

### 2. Build typio-linux

Point `PKG_CONFIG_PATH` at libtypio's `target/release` (where the `.pc`
files were generated) before running Meson.  If the variable is not set
and the package is not installed system-wide, Meson falls back to the
`subprojects/libtypio.wrap` and clones+builds libtypio automatically.

```bash
export PKG_CONFIG_PATH="../libtypio/target/release:${PKG_CONFIG_PATH}"
meson setup build --buildtype=debug -Denable_systray=true -Denable_voice=true
ninja -C build
```

### 3. Run the daemon while iterating

The built binary links against libtypio's `target/release/libtypio.so`, so
you must add that **same** directory to the dynamic linker path at runtime:

```bash
export LD_LIBRARY_PATH=../libtypio/target/release:${LD_LIBRARY_PATH}
./build/src/typio --verbose
```

The verbose log reports how many engines were discovered at startup
(`Host loader registered N engine(s) from …`).

For engine work, point the daemon at one or more manifest directories.
`--engine-dir` is repeatable and takes highest precedence, so you can run
freshly built engines straight from their build trees — no copy step, no
symlinking. Pass the directory that **contains** `typio-engine-*.toml`; the
scan is flat and does not recurse into subdirectories:

```bash
./build/src/typio -v \
  --engine-dir ../typio-engine-basic \
  --engine-dir ../typio-engine-mozc/build
```

Equivalently, set the colon-separated `$TYPIO_ENGINE_PATH` (PATH-style) once
in your shell instead of repeating the flag:

```bash
export TYPIO_ENGINE_PATH="$PWD/../typio-engine-basic:$PWD/../typio-engine-mozc/build"
./build/src/typio -v
```

The daemon auto-loads only from the system directory; `--engine-dir` and
`$TYPIO_ENGINE_PATH` are explicit opt-ins, and no per-user directory is
scanned by default (see [ADR-0025](../adr/0025-engine-discovery-search-path.md)).

## Load a keyboard engine (optional)

`typio` starts and runs without any keyboard engine — it just has nothing
to convert keystrokes with, so it logs a "No keyboard engine is active"
startup warning and falls back to a "No engine loaded" placeholder. To
actually exercise input conversion (and to verify your build wires up the
engine ABI correctly), build one engine and point the daemon at it.

Any engine works; this uses `typio-engine-basic`, the zero-dependency
fallback, as the concrete example. Cargo emits the engine executable and the
repository contains `typio-engine-basic.toml`:

```bash
( cd ../typio-engine-basic && cargo build --release )
```

This produces `../typio-engine-basic/target/release/typio-engine-basic`; the
manifest declares the `basic` engine identifier exposed to users and
configuration files.

Point the daemon at that build directory with `--engine-dir` — no copy and no
install step (see [Run the daemon while iterating](#3-run-the-daemon-while-iterating)):

```bash
./build/src/typio -v --engine-dir ../typio-engine-basic
```

Different engine packages may keep their manifest in the repository root or
generate it into `build`; pass the directory that contains the manifest.

## Optional features

| Option | Default | When you need it |
|---|---|---|
| `-Denable_systray=true` | `false` | System tray icon |
| `-Denable_voice=true` | `false` | PipeWire audio capture and voice session infrastructure |

`-Denable_voice=true` does **not** compile any voice engine into the binary.
Voice engines (Whisper, Sherpa-ONNX, …) are separate engine packages loaded at
runtime. This option only enables the host-side PipeWire capture and
voice-session plumbing that those external engines plug into. Without this
flag, voice engine manifests are rejected at load time with a
`voice_input capability` error because the host does not advertise the
required capability.

## Engine discovery

At startup `typio` scans a fixed, ordered list of directories and registers
the **first** engine of each name it finds. Production builds scan only the
system engine directory unless a development or test directory is enabled
explicitly.

| Order | Source | Path |
|---|---|---|
| 1 | `-E` / `--engine-dir DIR` | directories passed on the command line; repeatable, in the order given |
| 2 | `$TYPIO_ENGINE_PATH` | colon-separated list, in listed order |
| 3 | **System** | compile-time `<prefix>/<datadir>/typio/engines` (typically `/usr/share/typio/engines`) |

In each directory it loads only files named `typio-engine-*.toml`, parses the
manifest, and registers the declared engine argv with libtypio. The `name`
field becomes the engine identifier exposed in config and the command-line interface (`basic`,
`rime`, `sherpa`, …). A sibling `icons/` directory (`<engine-dir>/icons/`,
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
| `enable_systray` | `false` | Enable StatusNotifierItem support (requires libsystemd / sd-bus) |
| `enable_voice` | `false` | Enable PipeWire audio capture and voice session infrastructure |
| `enable_asan` | `false` | Enable AddressSanitizer |
| `enable_ubsan` | `false` | Enable UndefinedBehaviorSanitizer |

## Project layout

For a tour of the documentation structure, see [docs/index.md](../index.md).

## Submitting changes

See the [Pull Request Checklist](../../CONTRIBUTING.md#pull-request-checklist) in `CONTRIBUTING.md`.
