# CLI Reference

## Commands

| Command | Purpose |
|---------|---------|
| `typio` | Start the Wayland input method daemon. |

## Options

| Option | Argument | Effect |
|--------|----------|--------|
| `-c`, `--config` | `DIR` | Set the configuration directory. |
| `-d`, `--data` | `DIR` | Set the data directory. |
| `-E`, `--engine-dir` | `DIR` | Set the engine manifest directory override. |
| `-v`, `--verbose` | None | Enable debug logging. |
| `-vv` | None | Enable trace logging, including high-volume key-routing traces. |
| `-h`, `--help` | None | Print CLI help and exit. |
| `--version` | None | Print version information and exit. |

## Log Levels

| Invocation | Minimum log level |
|------------|-------------------|
| `typio` | `info` |
| `typio -v` | `debug` |
| `typio --verbose` | `debug` |
| `typio -vv` | `trace` |
| `typio --verbose --verbose` | `trace` |

## Runtime Signals

| Signal | Effect |
|--------|--------|
| `SIGUSR1` | Raise the running daemon log level by one step: `info` to `debug`, `debug` to `trace`. |
| `SIGUSR2` | Reset the running daemon log level to the startup level. |
