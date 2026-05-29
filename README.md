# typio-wayland

The Linux/Wayland host for the [Typio](https://github.com/) input method
framework. Installs the `typio` binary.

It embeds [libtypio](../libtypio) and provides the platform adapter layer:
the Wayland text-input/input-method v2 client, virtual-keyboard bridge,
the candidate popup (rendered with flux/Vulkan), the UDS control socket,
the D-Bus status interface, the StatusNotifierItem tray, and PipeWire
voice capture. It translates Wayland/D-Bus events into libtypio
abstractions and drives libtypio's callbacks back onto the compositor.

Plugin discovery is host-owned: at startup `typio` scans
`<libdir>/typio/engines` for `libtypio-engine-*.so`, `dlopen`s each, and
registers it with libtypio. Core itself contains no `dlopen` and no
engine paths.

## Building

Requires [libtypio](https://github.com/ming2k/libtypio) (either installed
system-wide or available via the meson wrap), Wayland, xkbcommon,
fontconfig/harfbuzz/freetype, D-Bus, and (for the popup) flux.

`libtypio` is resolved by pkg-config first.  Set `PKG_CONFIG_PATH` to point
at a local cargo build, or rely on a system install:

```sh
# Option A — pkg-config against a local libtypio checkout
export PKG_CONFIG_PATH="/path/to/libtypio/target/release:${PKG_CONFIG_PATH}"
meson setup build
ninja -C build

# Option B — let meson clone libtypio via the subproject wrap
meson setup build           # subprojects/libtypio.wrap is fetched and built
ninja -C build
```

See [`docs/dev/setup.md`](docs/dev/setup.md) for the full setup steps and
additional options.

Options: `-Denable_systray=true`, `-Denable_status_bus=true` (default),
`-Dbuild_voice=true` for voice input support. Voice engines (Whisper,
Sherpa-ONNX, …) are loaded as plugin `.so` files at runtime; this option
only enables the host-side PipeWire capture infrastructure.

## Running

```sh
export LD_LIBRARY_PATH=/path/to/libtypio/target/release:$LD_LIBRARY_PATH
typio --list                   # list discovered engines
typio --verbose                # run with debug logging
```

Engines are discovered from `~/.local/share/typio/engines` and
`/usr/local/lib/typio/engines`. Build the [basic engine](../typio-engine-basic)
with `cargo build --release` and copy the `.so` (renamed to
`libtypio-engine-*.so`) into one of those directories.

Control it from a separate terminal with the [typio-cli](../typio-cli)
client (`typio status`, `typio engine`, …).
