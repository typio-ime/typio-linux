//! Runtime diagnostics and structured logging setup.

use std::sync::OnceLock;

use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialize structured logging once for the daemon process.
///
/// `RUST_LOG` is the primary filter surface. Without it, the daemon keeps
/// diagnostics quiet unless `-v` / `-vv` is passed.
pub fn init_logging(verbosity: u8) {
    static INIT: OnceLock<()> = OnceLock::new();
    let _ = INIT.get_or_init(|| {
        let fallback = match verbosity {
            0 => "warn",
            1 => "info",
            _ => "debug",
        };
        let filter = EnvFilter::builder()
            .with_default_directive(LevelFilter::WARN.into())
            .from_env_lossy()
            .add_directive(
                fallback
                    .parse()
                    .unwrap_or_else(|_| LevelFilter::WARN.into()),
            );

        let layer = fmt::layer()
            .with_writer(std::io::stderr)
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false)
            .compact();

        tracing_subscriber::registry()
            .with(filter)
            .with(layer)
            .init();
    });
}
