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
LSAN_OPTIONS="suppressions=$(pwd)/tests/asan_suppressions.txt" \
    meson test -C build-asan --print-errorlogs
```

The `LSAN_OPTIONS` export is required locally: LeakSanitizer reports
internal libfontconfig leaks (pattern-matched in
`tests/asan_suppressions.txt`) that are not typio's. CI sets the same
variable in `.github/workflows/ci.yml`; without it `test_icon_badge`
and `test_font_resolve_purge` fail with 320-byte leaks in
`FcPatternObjectInsertElt` / `FcPatternObjectAddWithBinding`.

Use `dbus-run-session` for sanitizer and CI-like runs so status-bus and tray tests get an isolated session bus instead of depending on the developer's desktop session.

## Fuzzing

`src/ipc/tip_json.c` parses bytes received from UDS clients and has a
libFuzzer harness. Build it with clang and run it directly; it is not part
of `meson test`:

```bash
CC=clang meson setup build-fuzz -Denable_fuzzers=true
ninja -C build-fuzz tests/fuzz_tip_json
mkdir -p corpus
./build-fuzz/tests/fuzz_tip_json corpus/ -max_total_time=300
```

Run the harness after any change to `tip_json.c`, and add a fuzzer for any
new code that parses external input.

## Useful individual binaries

```bash
export LD_LIBRARY_PATH=/absolute/path/to/libtypio/target/release:${LD_LIBRARY_PATH}
./build/tests/test_key_arbiter
./build/tests/test_key_route
./build/tests/test_focus_controller
./build/tests/test_state_machine_properties
./build/tests/test_boundary_bridge
./build/tests/test_status_bus
```

## Known test failures

A small number of legacy host tests assume the **basic** keyboard engine is
available inside the test process. Because `typio-linux` loads engines as
out-of-process engine workers, unit tests that
do not explicitly register a mock engine or set `TYPIO_ENGINE_PATH` will not
see **basic** and may fail or time out. This is a test-harness limitation,
not a product bug.

The legacy tests are disabled in `tests/meson.build` until they are ported
to the registry-based plugin path.

If you need these tests to pass locally, set `TYPIO_ENGINE_PATH` to a
directory containing `typio-engine-basic.toml` before running the suite.

## Test ownership

Add or update tests when changing:

- Wayland lifecycle, key routing, repeat, or startup guard behavior
- runtime config reload, config-watch debounce, or event-loop scheduling
- voice service state transitions, reload deferral, or completion dispatch
- status/tray D-Bus dispatch loops
- candidate Panel layout, rendering, or state classification
- focus-controller `reduce` / `diff` / guard predicates (`test_focus_controller`, `test_state_machine_properties`)

Prefer small state-policy tests for Wayland behavior. Do not rely only on manual compositor testing when a bug can be reduced to a helper or state model.

## Style

- Use C23 for all code.
- Keep public API names in the `typio_*` / `Typio*` style already used by the repo.
- Prefer local helpers and direct data flow over broad abstractions.
- Document non-obvious behavior in headers or near complex state transitions.
- Keep generated protocol and renderer details behind narrow module boundaries.
