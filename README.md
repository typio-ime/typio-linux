# typio-wayland

The Linux/Wayland host for the [Typio](https://github.com/) input method
framework. Installs the `typio` binary.

It embeds [libtypio](../libtypio) and provides the platform adapter layer:
the Wayland text-input/input-method v2 client, virtual-keyboard bridge,
the candidate Panel (rendered with flux/Vulkan), the UDS control socket,
the D-Bus status interface, the StatusNotifierItem tray, and PipeWire
voice capture. It translates Wayland/D-Bus events into libtypio
abstractions and drives libtypio's callbacks back onto the compositor.

Plugin discovery is host-owned: at startup `typio` scans
`<libdir>/typio/engines` for `libtypio_engine_*.so`, `dlopen`s each, and
registers it with libtypio. Core itself contains no `dlopen` and no
engine paths.

## Building

Requires [libtypio](https://github.com/ming2k/libtypio) (either installed
system-wide or available via the meson wrap), Wayland, xkbcommon,
fontconfig/harfbuzz/freetype, D-Bus, and (for the Panel) flux.

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
`-Denable_voice=true` for voice input support. Voice engines (Whisper,
Sherpa-ONNX, …) are loaded as plugin `.so` files at runtime; this option
only enables the host-side PipeWire capture infrastructure.

## Running

```sh
export LD_LIBRARY_PATH=/path/to/libtypio/target/release:$LD_LIBRARY_PATH
typio --verbose                # run the daemon with debug logging
```

`typio` is the daemon. Inspecting and controlling a running instance (engines,
config, status) is the job of the separate `typioctl` client, which talks to
the daemon over its UDS socket.

Installed packages start the daemon through the systemd user service:

```sh
systemctl --user enable --now typio.service
journalctl --user -u typio -f
```

Engines are discovered from the system engine directory
`<prefix>/<libdir>/typio/engines`. Build the [basic engine](../typio-engine-basic)
with `cargo build --release` and install the `.so`
(`libtypio_engine_*.so`) into that directory. For development and testing,
pass `--engine-dir DIR` or set `TYPIO_ENGINE_DIR`.

Control it from a separate terminal with the [typioctl](../typioctl) client.
