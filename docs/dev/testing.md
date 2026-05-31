# Testing

This document is for contributors. It covers how to run and write tests.

## Run the test suite

```bash
meson test -C build --print-errorlogs
```

When running tests locally, make sure the test runner can find `libtypio.so`
and that engine plugin directories are visible if a test needs them:

```bash
export LD_LIBRARY_PATH=/absolute/path/to/libtypio/target/release:${LD_LIBRARY_PATH}
meson test -C build --print-errorlogs
```

Run with an isolated D-Bus session when validating status-bus, tray, or CI-like behavior:

```bash
dbus-run-session -- meson test -C build --print-errorlogs
```

Run sanitizer coverage:

```bash
meson setup build-asan --buildtype=debug -Denable_asan=true -Denable_ubsan=true
ninja -C build-asan
meson test -C build-asan --print-errorlogs
```

Use `dbus-run-session` for sanitizer and CI-like runs so status-bus and tray tests get an isolated session bus instead of depending on the developer's desktop session.

## Useful individual binaries

```bash
export LD_LIBRARY_PATH=/absolute/path/to/libtypio/target/release:${LD_LIBRARY_PATH}
./build/tests/test_key_arbiter
./build/tests/test_key_route
./build/tests/test_reconciler
./build/tests/test_boundary_bridge
./build/tests/test_status_bus
```

## Known test failures

A small number of legacy host tests assume the **basic** keyboard engine is
available inside the test process. Because `typio-wayland` loads engines as
host-side plugins (via `dlopen` of `libtypio_engine_*.so`), unit tests that
do not explicitly register a mock engine or set `TYPIO_ENGINE_DIR` will not
see **basic** and may fail or time out. This is a test-harness limitation,
not a product bug.

The legacy tests are disabled in `tests/meson.build` until they are ported
to the registry-based plugin path.

If you need these tests to pass locally, set `TYPIO_ENGINE_DIR` to a
directory containing `libtypio_engine_basic.so` before running the suite.

## Test ownership

Add or update tests when changing:

- Wayland lifecycle, key routing, repeat, or startup guard behavior
- runtime config reload, config-watch debounce, or event-loop scheduling
- voice service state transitions, reload deferral, or completion dispatch
- status/tray D-Bus dispatch loops
- candidate popup layout, rendering, or state classification
- reconciler divergence detection and repair logic

Prefer small state-policy tests for Wayland behavior. Do not rely only on manual compositor testing when a bug can be reduced to a helper or state model.

## Style

- Use C23 for all code.
- Keep public API names in the `typio_*` / `Typio*` style already used by the repo.
- Prefer local helpers and direct data flow over broad abstractions.
- Document non-obvious behavior in headers or near complex state transitions.
- Keep generated protocol and renderer details behind narrow module boundaries.
