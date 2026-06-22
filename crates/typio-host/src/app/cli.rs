//! Command-line parsing.
//!
//! Split out of `mod.rs` so the daemon lifecycle file owns *what the
//! options mean downstream* rather than *how they are parsed*. The
//! [`Cli`] struct mirrors `argv` via clap; [`AppOptions`] is the typed,
//! resolved form consumed by [`crate::app::App`].

use std::path::PathBuf;

use clap::Parser;

/// Raw command-line options for the typio daemon, as parsed from argv.
#[derive(Parser, Debug, Clone)]
#[command(name = "typio", version, about = "Typio Wayland input-method daemon")]
pub(super) struct Cli {
    /// Configuration directory.
    #[arg(short, long)]
    pub(super) config: Option<PathBuf>,
    /// Data directory.
    #[arg(short, long)]
    pub(super) data: Option<PathBuf>,
    /// Engine directory (repeatable; highest precedence).
    #[arg(short = 'E', long)]
    pub(super) engine_dir: Vec<PathBuf>,
    /// Unix-domain control socket path.
    #[arg(long)]
    pub(super) socket: Option<PathBuf>,
    /// Increase logging verbosity (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub(super) verbose: u8,
}

/// Runtime options after CLI parsing and directory resolution.
///
/// Strings are owned (not `&str` or `PathBuf`) because downstream
/// consumers (libtypio FFI, EngineLoader) need them as `String` anyway;
/// converting once at parse time keeps the hot paths free of path-to-string
/// coercions.
#[derive(Debug, Clone)]
pub struct AppOptions {
    pub config_dir: Option<String>,
    pub data_dir: Option<String>,
    pub engine_dirs: Vec<String>,
    pub socket_path: Option<PathBuf>,
    pub verbosity: u8,
}

impl From<Cli> for AppOptions {
    fn from(cli: Cli) -> Self {
        Self {
            config_dir: cli.config.map(|p| p.to_string_lossy().into_owned()),
            data_dir: cli.data.map(|p| p.to_string_lossy().into_owned()),
            engine_dirs: cli
                .engine_dir
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            socket_path: cli.socket,
            verbosity: cli.verbose,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_into_app_options() {
        let cli = Cli::parse_from([
            "typio",
            "-c",
            "/cfg",
            "--socket",
            "/sock",
            "-E",
            "/e1",
            "-E",
            "/e2",
            "-vv",
        ]);
        let opts: AppOptions = cli.into();
        assert_eq!(opts.config_dir, Some("/cfg".to_string()));
        assert_eq!(opts.data_dir, None);
        assert_eq!(opts.engine_dirs, vec!["/e1".to_string(), "/e2".to_string()]);
        assert_eq!(opts.socket_path, Some(PathBuf::from("/sock")));
        assert_eq!(opts.verbosity, 2);
    }
}
