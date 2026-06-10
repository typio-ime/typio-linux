# Contributing

## Build and test

Follow [Developer Setup](docs/dev/setup.md) for dependencies and the
libtypio checkout, then:

```bash
meson setup build
ninja -C build
meson test -C build --print-errorlogs
```

Before sending a change, run the suite under sanitizers as described in
[Testing](docs/dev/testing.md). CI enforces `-Dwerror=true`, ASan, and
UBSan, so a warning or a leak will fail the pull request.

## libtypio version

CI builds against the libtypio commit pinned by `LIBTYPIO_PINNED_REF` in
`.github/workflows/ci.yml`. If your change needs newer libtypio API, bump
the pin in its own commit and check that the `meson.build` version floor
still matches. The canary job tracks libtypio `main`; its failures are
informational and never block a pull request.

## Changes that need more than code

- **Architectural decisions** get an ADR. Follow the
  [ADR workflow](docs/dev/documentation/adr-workflow.md).
- **User-visible changes** get an entry under **Unreleased** in
  `CHANGELOG.md` ([Keep a Changelog](https://keepachangelog.com/) format).
- **Breaking changes to an external interface** must respect its tier in
  the [Interface Stability Reference](docs/reference/stability.md).
- **Documentation** follows the
  [documentation governance](docs/dev/documentation/index.md) rules; read
  the routing and style guide pages before adding or moving a doc.
- **Code style** is described in [Code Style](docs/dev/code-style.md).

## Tests

`docs/dev/testing.md` lists the subsystems that require test updates when
touched. New parsers of external input also need a fuzz harness (see the
Fuzzing section there).

## Security

Do not report vulnerabilities in public issues; see
[SECURITY.md](SECURITY.md).
