//! typio — unified Wayland input-method daemon (Rust).
//!
//! This is the shipping daemon entry point. It delegates the full lifecycle
//! to [`typio_host::app::App`].

use std::process::ExitCode;

use typio_host::app::App;

fn main() -> ExitCode {
    let mut app = match App::from_env() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };

    if let Err(e) = app.init() {
        eprintln!("typio: init failed: {e}");
        return ExitCode::from(1);
    }

    let exit_code = app.run();
    app.shutdown();
    let code = app.finish(exit_code);
    ExitCode::from(code as u8)
}
