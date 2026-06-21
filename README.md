# typio-linux

**Typio for Linux** — a Wayland-native input method host for the
[Typio](https://github.com/) input method framework. Installs the `typio`
binary.

> Currently Wayland-only (`text-input-v2` / `input-method-v2`). X11 is not
> supported and not planned — this host targets the modern Wayland desktop.

It embeds [libtypio](../libtypio) and provides the platform adapter layer:
the Wayland text-input/input-method v2 client, virtual-keyboard bridge,
the candidate Panel (rendered with flux/Vulkan), the UDS control socket,
the StatusNotifierItem tray, and PipeWire voice capture. It translates
Wayland events into libtypio abstractions and drives libtypio's callbacks
back onto the compositor. (The old D-Bus status interface was removed in
ADR-0008; the tray speaks SNI over D-Bus via zbus when the `systray`
Cargo feature is enabled.)

Engine discovery is host-owned: at startup `typio` scans
`<datadir>/typio/engines` for `typio-engine-*.toml` manifests and registers
direct worker processes with libtypio. Core itself contains no engine search
paths.

## Building

Requires [libtypio](https://github.com/ming2k/libtypio), Wayland, xkbcommon,
fontconfig/harfbuzz/freetype, PipeWire for voice capture, and flux for the
candidate Panel.

The host build is Cargo. `flux` is still a native C library, so build the
sibling flux checkout first until flux has its own Cargo-native library
build.

```bash
# Build the native dependencies first (from the typio-linux repo root).
cargo build --release --manifest-path ../libtypio/Cargo.toml
meson setup ../../flux/build ../../flux    # one-time per flux checkout
meson compile -C ../../flux/build

export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
cargo build --release -p typio-host
cargo test -p typio-host
```

The `typio` binary is produced at `target/release/typio`. Install it along
with the systemd service, icons, and example configs with `cargo xtask install`.

See [`docs/dev/setup.md`](docs/dev/setup.md) for the full setup steps and
additional options.

Cargo features: `--features systray` enables the StatusNotifierItem tray
(via zbus). `--features voice` enables PipeWire audio capture and the
`voice_input` host capability; voice engines run as worker processes at
runtime, this option only enables the host-side capture infrastructure.

## Running

```bash
export LD_LIBRARY_PATH="$PWD/../libtypio/target/release:$PWD/../../flux/build:${LD_LIBRARY_PATH}"
typio --verbose                # run the daemon with debug logging
```

`typio` is the daemon. Inspecting and controlling a running instance (engines,
config, status) is the job of the separate `typioctl` client, which talks to
the daemon over its UDS socket.

Installed packages start the daemon through the systemd user service:

```bash
systemctl --user enable --now typio.service
journalctl --user -u typio -f
```

Engines are discovered from the system engine directory
`<prefix>/<datadir>/typio/engines`. Build a sibling engine such as
[compose](../typio-engine-compose) (`cargo build --release`) or
[rime](../typio-engine-rime) (`meson setup build && meson compile -C build`)
and install its `typio-engine-*.toml` into that directory. For development
and testing, pass `--engine-dir DIR` or set `TYPIO_ENGINE_PATH`.

Control it from a separate terminal with the [typioctl](../typioctl) client.
