# typio-linux

**Typio for Linux** — a Wayland-native input method host for the
[Typio](https://github.com/) input method framework. Installs the `typio`
binary.

> Currently Wayland-only (`text-input-v2` / `input-method-v2`). X11 is not
> supported and not planned — this host targets the modern Wayland desktop.

It embeds [libtypio](../libtypio) and provides the platform adapter layer:
the Wayland text-input/input-method v2 client, virtual-keyboard bridge,
the candidate Panel (rendered with flux/Vulkan), the UDS control socket,
the D-Bus status interface, the StatusNotifierItem tray, and PipeWire
voice capture. It translates Wayland/D-Bus events into libtypio
abstractions and drives libtypio's callbacks back onto the compositor.

Engine discovery is host-owned: at startup `typio` scans
`<datadir>/typio/engines` for `typio-engine-*.toml` manifests and registers
direct worker processes with libtypio. Core itself contains no engine search
paths.

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
`-Denable_voice=true` for voice input support. Voice engines run as worker
processes at runtime; this option only enables the host-side PipeWire capture
infrastructure.

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
`<prefix>/<datadir>/typio/engines`. Build the [basic engine](../typio-engine-basic)
with `cargo build --release` and install `typio-engine-basic.toml` into that
directory. For development and testing, pass `--engine-dir DIR` or set
`TYPIO_ENGINE_PATH`.

Control it from a separate terminal with the [typioctl](../typioctl) client.
