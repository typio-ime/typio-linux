//! Top-level daemon lifecycle.
//!
//! Port of `src/app.c` + `src/cli.c` + `src/main.c`. Owns the `TypioInstance`,
//! wires engine loading, signal handling, restart-on-exec, and the eventual
//! Wayland frontend / tray / IPC surfaces.

mod cli;
mod event_loop;
mod indicator;
mod signals;
mod tray;

#[cfg(feature = "systray")]
use tray::{build_tray_snapshot, install_tray_action_handler, update_tray_from_controller};

use std::cell::RefCell;
use std::ffi::{c_char, CString};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use clap::Parser;
use typio::c_api::registry as c_registry;
use typio::instance::TypioInstance;

use cli::Cli;
pub use cli::AppOptions;

use crate::config_watcher::ConfigWatcher;
use crate::engine_loader::resolve_engine_dirs;
use crate::indicator::{Indicator, IndicatorConfig};
use crate::ipc::protocol;
use crate::ipc::protocol::topics;
use crate::ipc_bus::{IpcBus, TypioBackend, TypioRegistryView};
use crate::resume_signal::ResumeSignal;
use crate::session_glue::FocusDriver;
use crate::state_controller::{StateChange, StateController};
use crate::tray_sni::Tray;
use crate::uds_server::UdsServer;
use crate::watchdog::Watchdog;

#[cfg(feature = "wayland")]
use nix::sys::timerfd::{ClockId, TimerFd as NixTimerFd, TimerFlags};

#[cfg(feature = "wayland")]
use crate::input_method::InputMethodFrontend;
#[cfg(feature = "wayland")]
use crate::keyboard::router::KeyboardRouter;
#[cfg(feature = "wayland")]
use crate::repeat_timer::{self, RepeatTimer};

/// Cross-thread events delivered to the main loop.
///
/// Senders live in:
/// - the IPC stop callback (UDS `daemon.stop` method),
/// - the StatusNotifierItem tray action callback (zbus internal thread).
///
/// The receiver is owned by [`App`] and drained once per tick by the
/// main loop. This keeps every mutation of `App` state on the
/// event-loop thread — the alternative (`AtomicBool` flags for each
/// cause) loses type information and forces the loop to do untyped
/// "refresh everything" work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonEvent {
    /// Cleanly stop the daemon. Causes the main loop to exit.
    Shutdown,
    /// Stop and re-exec with the same argv. Causes the main loop to
    /// exit; [`App::finish`] then `execv`s.
    Restart,
    /// libtypio state changed (engine / language / voice switch from
    /// the tray). Re-sync `StateController`, IPC bus, and tray surface.
    StateRefresh,
}

/// The running daemon.
pub struct App {
    argv: Vec<CString>,
    options: AppOptions,
    instance: Option<Box<TypioInstance>>,
    state_controller: Option<StateController<TypioRegistryView>>,
    ipc_bus: Option<Rc<RefCell<IpcBus>>>,
    #[cfg(feature = "systray")]
    tray: Option<Tray>,
    #[cfg(feature = "wayland")]
    frontend: Option<InputMethodFrontend>,
    #[cfg(feature = "wayland")]
    router: Option<KeyboardRouter>,
    #[cfg(feature = "wayland")]
    repeat_timer: Option<RepeatTimer>,
    #[cfg(feature = "wayland")]
    resume_signal: Option<ResumeSignal>,
    #[cfg(feature = "wayland")]
    focus_driver: Option<FocusDriver>,
    /// On-screen indicator state machine (gate state + label composition).
    /// Pure; the popup surface is owned by `PanelCoordinator`, the auto-hide
    /// timer by [`Self::indicator_timer`].
    indicator: Option<Indicator>,
    /// Cached indicator configuration snapshot. Re-read from libtypio on
    /// startup and on every config reload so the running loop never does
    /// FFI on the hot path.
    indicator_config: IndicatorConfig,
    /// Auto-hide timerfd for the indicator. Armed when the indicator
    /// actually becomes visible (coordinator accepted the show); disarmed
    /// on hide, focus-loss, or shutdown. Polled as part of the main poll
    /// set; expiry drives `indicator.hide()` + panel detach.
    #[cfg(feature = "wayland")]
    indicator_timer: Option<NixTimerFd>,
    /// Absolute time when the indicator should auto-hide, mirroring the
    /// kernel timerfd state. Tracked in user space so the poll timeout
    /// can be lowered without a `timerfd_gettime` syscall on every tick.
    #[cfg(feature = "wayland")]
    indicator_hide_deadline: Option<Instant>,
    config_watcher: Option<ConfigWatcher>,
    watchdog: Option<Watchdog>,
    /// Sender half of the daemon event channel. Cloned into the IPC
    /// stop callback and the tray action handler.
    event_tx: std::sync::mpsc::Sender<DaemonEvent>,
    /// Receiver half of the daemon event channel. Drained once per tick
    /// by the main loop; never shared with another thread (`Receiver` is
    /// `!Sync`).
    event_rx: Option<std::sync::mpsc::Receiver<DaemonEvent>>,
    /// Observed `DaemonEvent::Restart` during the last drain. Consumed
    /// by [`Self::finish`] to decide whether to `execv` after exit.
    saw_restart: bool,
}

impl App {
    /// CLI verbosity selected at startup.
    pub fn verbosity(&self) -> u8 {
        self.options.verbosity
    }

    /// Parse CLI args and create an uninitialized app shell.
    pub fn from_env() -> Result<Self, String> {
        // `parse()` handles --help and --version by printing and exiting with
        // code 0, matching standard CLI conventions.
        let cli = Cli::parse();
        let options = AppOptions::from(cli);
        let argv: Vec<CString> = std::env::args()
            .map(CString::new)
            .collect::<Result<_, _>>()
            .map_err(|_| "argument contains NUL".to_string())?;
        let (event_tx, event_rx) = std::sync::mpsc::channel::<DaemonEvent>();
        Ok(Self {
            argv,
            options,
            instance: None,
            state_controller: None,
            ipc_bus: None,
            #[cfg(feature = "systray")]
            tray: None,
            #[cfg(feature = "wayland")]
            frontend: None,
            #[cfg(feature = "wayland")]
            router: None,
            #[cfg(feature = "wayland")]
            repeat_timer: None,
            #[cfg(feature = "wayland")]
            resume_signal: None,
            #[cfg(feature = "wayland")]
            focus_driver: None,
            indicator: None,
            indicator_config: IndicatorConfig::default(),
            #[cfg(feature = "wayland")]
            indicator_timer: None,
            #[cfg(feature = "wayland")]
            indicator_hide_deadline: None,
            config_watcher: None,
            watchdog: None,
            event_tx,
            event_rx: Some(event_rx),
            saw_restart: false,
        })
    }

    /// Default config directory: `$XDG_CONFIG_HOME/typio` or `~/.config/typio`.
    fn default_config_dir() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join(".config")
            })
            .join("typio")
    }

    /// Initialize the Typio instance and load engines.
    pub fn init(&mut self) -> Result<(), String> {
        // Engine search path: CLI dirs > $TYPIO_ENGINE_PATH > system dir.
        let engine_dirs = resolve_engine_dirs(self.options.engine_dirs.iter().cloned());

        let mut instance = TypioInstance::new_rust(
            self.options.config_dir.as_deref(),
            self.options.data_dir.as_deref(),
            None, // state_dir — let libtypio pick the default.
            engine_dirs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
        );

        instance
            .init_rust()
            .map_err(|e| format!("TypioInstance init failed: {e:?}"))?;

        // Set up the config watcher so the event loop can react to file changes.
        let config_dir = self
            .options
            .config_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(Self::default_config_dir);
        self.config_watcher = ConfigWatcher::new(&config_dir).ok();
        if let Some(ref mut watcher) = self.config_watcher {
            let engines_dir = config_dir.join("engines");
            if engines_dir.is_dir() {
                let _ = watcher.watch_engines_dir(&engines_dir);
            }
        }

        // Register engines from the resolved directories via libtypio's
        // native Rust API (ADR-0035). EngineLoader handles manifest
        // discovery, parsing, capability negotiation, and ProcessBackend
        // registration in one pass — bypassing the C ABI used by the
        // legacy `typio_registry_register_engine_process` path.
        let raw = instance.as_mut() as *mut TypioInstance;
        let registry = typio::instance::typio_instance_get_registry(raw);
        if registry.is_null() {
            return Err("engine registry not available".to_string());
        }

        let mut loader = crate::engine_loader::EngineLoader::with_voice();
        let mut registered_keyboards: Vec<String> = Vec::new();
        let mut registered_voices: Vec<String> = Vec::new();
        for dir in &engine_dirs {
            let dir_path = std::path::Path::new(dir);
            if !dir_path.is_dir() {
                continue;
            }
            // SAFETY: `raw` is a valid, initialised `*mut TypioInstance`;
            // `registry_rust_mut` borrows `&mut instance` exclusively for
            // the duration of the call. The C-ABI `registry` pointer
            // borrowed above is not dereferenced in this block.
            let Some(reg) = instance.registry_rust_mut() else {
                continue;
            };
            let report = loader.load_dir(reg, dir_path);
            for info in report.registered {
                eprintln!(
                    "OK:   registered engine '{}' from {}/",
                    info.name,
                    dir_path.display()
                );
                if info.engine_type == typio::core::engine::EngineType::Voice {
                    registered_voices.push(info.name);
                } else {
                    registered_keyboards.push(info.name);
                }
            }
            for (path, reason) in &report.skipped {
                eprintln!("WARN: skipped {}: {reason:?}", path.display());
            }
            for (path, err) in &report.failed {
                eprintln!("WARN: failed to load {}: {err}", path.display());
            }
        }

        // Restore the persisted language (last-used if still enabled,
        // otherwise the first enabled language). This both activates the
        // matching keyboard/voice engines for that language and sets
        // `active_language` so the tray icon shows the right badge (中 / EN /
        // あ …) instead of the generic `typio-keyboard-symbolic`. Falls back
        // to the first registered keyboard when no languages are declared so
        // legacy layout-only setups keep working.
        let restored = c_registry::typio_registry_restore_language(registry);
        if restored != typio::TypioResult::TypioOk {
            if let Some(first) = registered_keyboards.first() {
                if let Ok(c_name) = CString::new(first.as_str()) {
                    c_registry::typio_registry_set_active_keyboard(
                        registry,
                        c_name.as_ptr(),
                    );
                    eprintln!("OK:   active keyboard = {first}");
                }
            }
        } else {
            eprintln!("OK:   language restored");
        }
        eprintln!(
            "OK:   registered {} keyboard(s){}",
            registered_keyboards.len(),
            if registered_voices.is_empty() {
                String::new()
            } else {
                format!(", {} voice(s)", registered_voices.len())
            }
        );

        self.instance = Some(instance);

        // Wire the mode-changed callback so engine-internal mode switches
        // (rime schema changes, 中/A toggle, etc.) reach the indicator.
        // The trampoline's sender lives in `signals::MODE_CALLBACK_TX` so
        // the callback (which fires on the engine-comm thread for
        // out-of-process engines like rime) can safely reach the main
        // loop. Without this, only Ctrl+Shift engine switches trigger
        // the indicator — rime's own mode/schema switches are silent.
        signals::set_mode_callback_tx(self.event_tx.clone());
        {
            let raw = self.instance.as_ref().unwrap().as_ref() as *const TypioInstance as *mut TypioInstance;
            typio::instance::typio_instance_set_keyboard_mode_changed_callback(
                raw,
                signals::mode_changed_trampoline as _,
                std::ptr::null_mut(),
            );
        }

        #[cfg(feature = "wayland")]
        {
            match InputMethodFrontend::connect(None) {
                Ok(frontend) => {
                    eprintln!("OK: Wayland input-method frontend connected");
                    self.frontend = Some(frontend);
                }
                Err(e) => {
                    eprintln!("WARN: Wayland frontend not available: {e}");
                }
            }

            if let Some(ref mut instance) = self.instance {
                let raw = instance.as_mut() as *mut TypioInstance;
                match unsafe { KeyboardRouter::new(raw) } {
                    Some(router) => {
                        if let Some(ref mut frontend) = self.frontend {
                            frontend.set_input_context(router.ctx());
                        }
                        self.router = Some(router);
                    }
                    None => {
                        eprintln!("WARN: failed to create keyboard router");
                    }
                }

                match RepeatTimer::new() {
                    Ok(timer) => {
                        self.repeat_timer = Some(timer);
                    }
                    Err(e) => {
                        eprintln!("WARN: failed to create repeat timer: {e}");
                    }
                }

                self.resume_signal = Some(ResumeSignal::new());
                self.focus_driver = Some(FocusDriver::new());

                // Indicator subsystem: state machine + auto-hide timerfd.
                // The timer is created disarmed and only armed when a show
                // actually lands on screen (see `arm_indicator_timer`).
                self.indicator = Some(Indicator::new());
                match NixTimerFd::new(ClockId::CLOCK_MONOTONIC, TimerFlags::TFD_NONBLOCK) {
                    Ok(tf) => self.indicator_timer = Some(tf),
                    Err(e) => eprintln!("WARN: failed to create indicator timer: {e}"),
                }
            }

            self.indicator_config = self.load_indicator_config();
        }

        let raw = self.instance.as_mut().unwrap().as_mut() as *mut TypioInstance;
        self.state_controller = Some(StateController::new(TypioRegistryView::new(raw)));

        #[cfg(feature = "systray")]
        {
            let mut tray = Tray::new();
            let registered = tray.register();
            if registered {
                eprintln!(
                    "OK:   StatusNotifierItem registered as {}",
                    tray.service_name()
                );
            } else {
                eprintln!(
                    "WARN: tray did not register (no org.kde.StatusNotifierWatcher on the session bus?)"
                );
            }
            install_tray_action_handler(&tray, raw, self.event_tx.clone());
            if let Some(snapshot) = build_tray_snapshot(raw) {
                tray.set_menu_snapshot(snapshot);
            }
            self.tray = Some(tray);
        }

        Ok(())
    }

    /// Run the daemon until shutdown.
    pub fn run(&mut self) -> i32 {
        if self.instance.is_none() {
            eprintln!("typio: app not initialized");
            return 1;
        }

        signals::install_signal_handlers();

        let socket_path = self
            .options
            .socket_path
            .clone()
            .unwrap_or_else(protocol::socket_path);
        let server = match UdsServer::bind(&socket_path) {
            Ok(s) => {
                eprintln!("OK: UDS listening on {}", socket_path.display());
                s
            }
            Err(e) => {
                eprintln!("WARN: UDS bind failed: {e} — running without IPC");
                return self.run_without_uds();
            }
        };

        self.print_startup_banner();

        let raw = self.instance.as_mut().unwrap().as_mut() as *mut TypioInstance;
        let backend = TypioBackend::new(raw);
        let service = crate::service::StatusService::new(backend);
        let ipc_bus = Rc::new(RefCell::new(IpcBus::new(server, service)));
        // The IPC `daemon.stop` method routes through the same event
        // channel as tray actions — sending Shutdown here makes the main
        // loop the single place that decides when to exit.
        let stop_tx = self.event_tx.clone();
        ipc_bus.borrow_mut().set_stop_callback(move || {
            let _ = stop_tx.send(DaemonEvent::Shutdown);
        });

        // IPC-driven mutations (engine/language switch, config reload, engine
        // load/unload) bypass the Rust `StateController` notification path —
        // the registry is mutated directly via the C ABI. Route a
        // `StateRefresh` back to the main loop so derived surfaces (controller
        // snapshot, tray icon, tooltip, menu) re-sync against the new state.
        // Without this, `typioctl language use en` would update the registry
        // but leave the tray badge showing the previous language.
        let state_tx = self.event_tx.clone();
        ipc_bus.borrow_mut().set_state_change_callback(move || {
            let _ = state_tx.send(DaemonEvent::StateRefresh);
        });

        if let Some(ref mut controller) = self.state_controller {
            let ipc = ipc_bus.clone();
            controller.add_listener(Box::new(move |change| {
                let (topic, payload) = match change {
                    StateChange::Engine | StateChange::VoiceEngine => {
                        (topics::ENGINE_CHANGED, serde_json::json!({}))
                    }
                    StateChange::Language => (topics::LANGUAGE_CHANGED, serde_json::json!({})),
                    _ => (topics::RUNTIME_CHANGED, serde_json::json!({})),
                };
                ipc.borrow_mut().emit(topic, &payload);
            }));
            controller.sync();

            #[cfg(feature = "systray")]
            if let Some(ref tray) = self.tray {
                update_tray_from_controller(tray, controller, raw);
            }
        }

        self.ipc_bus = Some(ipc_bus.clone());

        eprintln!("typio: running. Ctrl+C to exit.");

        #[cfg(feature = "wayland")]
        if self.frontend.is_some() && self.router.is_some() && self.repeat_timer.is_some() {
            let watchdog = Watchdog::start();
            watchdog.set_armed(true);
            self.watchdog = Some(watchdog);
            return self.run_with_wayland(&ipc_bus);
        }

        self.run_with_uds(&ipc_bus)
    }

    fn run_with_uds(&mut self, ipc_bus: &Rc<RefCell<IpcBus>>) -> i32 {
        let uds_fd = ipc_bus.borrow().epoll_fd();
        let mut pollfd = libc::pollfd {
            fd: uds_fd,
            events: libc::POLLIN,
            revents: 0,
        };

        while !self.drain_events() {
            ipc_bus.borrow_mut().dispatch();
            pollfd.revents = 0;
            let rc = unsafe { libc::poll(&mut pollfd, 1, 100) };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                eprintln!("poll error: {e}");
                return 1;
            }
        }

        eprintln!("typio: shutting down...");
        0
    }

    #[cfg(feature = "wayland")]

    /// Reload core/platform configuration and notify listeners.
    fn reload_config(&mut self) {
        let Some(ref mut instance) = self.instance else {
            return;
        };
        let raw = instance.as_mut() as *mut TypioInstance;
        match typio::instance::typio_instance_reload_config(raw) {
            typio::TypioResult::TypioOk => {
                eprintln!("typio: configuration reloaded");
                self.indicator_config = self.load_indicator_config();
                self.refresh_state_surfaces();
            }
            _ => eprintln!("typio: configuration reload failed"),
        }
    }
    /// Re-sync the Rust-side `StateController` with libtypio, then push
    /// the resulting state to every surface that mirrors it: the IPC
    /// bus (controller listeners), the tray icon + tooltip, and the
    /// tray menu snapshot.
    ///
    /// Called from two paths: the config-watcher reload callback (config
    /// may have changed the active engine/language), and the main-loop
    /// drain of `DaemonEvent::StateRefresh` (tray-driven engine/language
    /// switches that bypass the Rust controller).
    fn refresh_state_surfaces(&mut self) {
        let raw = match self.instance.as_mut() {
            Some(inst) => inst.as_mut() as *mut TypioInstance,
            None => return,
        };
        if let Some(ref mut controller) = self.state_controller {
            controller.sync();
            #[cfg(feature = "systray")]
            if let Some(ref tray) = self.tray {
                update_tray_from_controller(tray, controller, raw);
            }
        }
        if let Some(ref ipc) = self.ipc_bus {
            ipc.borrow_mut()
                .emit(topics::RUNTIME_CHANGED, &serde_json::json!({}));
        }
        #[cfg(feature = "systray")]
        if let Some(ref tray) = self.tray {
            if let Some(snapshot) = build_tray_snapshot(raw) {
                tray.set_menu_snapshot(snapshot);
            }
        }
    }

    fn run_without_uds(&mut self) -> i32 {
        eprintln!("typio: running without UDS");

        #[cfg(feature = "wayland")]
        if let Some(ref mut frontend) = self.frontend {
            let _ = frontend.run();
            return 0;
        }

        while !self.drain_events() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        0
    }

    /// Drain all pending daemon events and translate the signal-flag into
    /// the same model. Returns the resulting action set so the main loop
    /// can break / refresh / nothing in one place.
    ///
    /// - `Shutdown` and the SIGINT/SIGTERM flag both set `should_exit`.
    /// - `Restart` additionally records `saw_restart` for [`Self::finish`].
    /// - `StateRefresh` triggers a controller + tray + IPC re-sync via
    ///   [`Self::refresh_state_surfaces`].
    fn drain_events(&mut self) -> bool {
        let mut should_exit = signals::take_shutdown_requested();
        let mut state_refresh = false;

        if let Some(rx) = self.event_rx.as_ref() {
            for event in rx.try_iter() {
                match event {
                    DaemonEvent::Shutdown => should_exit = true,
                    DaemonEvent::Restart => {
                        self.saw_restart = true;
                        should_exit = true;
                    }
                    DaemonEvent::StateRefresh => state_refresh = true,
                }
            }
        }

        if state_refresh {
            eprintln!("indicator: StateRefresh received, refreshing state surfaces");
            self.refresh_state_surfaces();
            // StateRefresh covers every deliberate registry mutation:
            // Ctrl+Shift engine-switch chord, tray menu picks, and
            // IPC-driven switches (`typioctl language use …`). All are
            // user-initiated, so they go through the indicator's
            // no-gate deliberate-change path.
            #[cfg(feature = "wayland")]
            if self.frontend.is_some() {
                self.trigger_indicator_state_change();
            }
        }

        should_exit
    }

    /// Tear down runtime services.
    ///
    /// Drops the dependents that hold raw pointers into `TypioInstance`
    /// (router, frontend, repeat timer, controller) BEFORE the instance
    /// itself, so their `Drop` impls see valid memory. Without this the
    /// instance drops first inside `self.instance.take()`, frees the
    /// `TypioInputContext` (it owns all contexts), and then the router's
    /// own `Drop` calls `typio_input_context_free` on a dangling pointer
    /// → double-free segfault.
    pub fn shutdown(&mut self) {
        drop(self.router.take());
        drop(self.repeat_timer.take());
        drop(self.frontend.take());
        drop(self.state_controller.take());
        if let Some(mut instance) = self.instance.take() {
            instance.shutdown_rust();
        }
    }

    /// Finalize: exec on restart, then return the exit code.
    pub fn finish(self, exit_code: i32) -> i32 {
        if self.saw_restart && exit_code == 0 {
            eprintln!("typio: restarting...");
            let argv0 = self
                .argv
                .first()
                .cloned()
                .unwrap_or_else(|| CString::new("typio").unwrap());
            let mut ptrs: Vec<*const c_char> = self.argv.iter().map(|s| s.as_ptr()).collect();
            ptrs.push(std::ptr::null());
            unsafe {
                libc::execv(argv0.as_ptr(), ptrs.as_ptr());
            }
            eprintln!("typio: execv failed: {}", std::io::Error::last_os_error());
            return 1;
        }
        exit_code
    }

    fn print_startup_banner(&self) {
        let version = env!("CARGO_PKG_VERSION");
        eprintln!("Starting typio {version}");
    }
}

/// Arm or disarm the keyboard repeat timer based on the current modifier
/// state and the compositor's reported repeat preferences.
///
/// Used by the main loop after both the engine-consumed and
/// forwarded-key paths so both kinds of key repeat identically.
/// Auto-repeat is suppressed entirely when a repeat-suppressing
/// modifier (Ctrl / Alt / Super) is held, or when the compositor
/// advertises `rate == 0`.
#[cfg(feature = "wayland")]
fn arm_repeat(timer: &mut RepeatTimer, compositor_info: Option<(i32, i32)>, mods_depressed: u32) {
    if !repeat_timer::should_repeat_for_modifiers(repeat_timer::Modifiers(mods_depressed)) {
        let _ = timer.stop();
        return;
    }
    match repeat_timer::resolve_repeat_params(compositor_info) {
        Some((delay, interval)) => {
            let _ = timer.start(delay, interval);
        }
        None => {
            // Compositor reports rate == 0: do not repeat.
            let _ = timer.stop();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    /// Serialises tests that touch the shared signal flags so they do not
    /// race with each other when `cargo test` runs them in parallel.
    static SIGNAL_FLAG_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn daemon_events_drive_drain_results() {
        let _guard = SIGNAL_FLAG_LOCK.lock().unwrap();

        // Reset the signal flag so prior tests don't leak.
        signals::reset_shutdown_flag();

        // Build a minimal App with just the event channel wired. Other
        // fields are empty; drain_events does not touch them unless an
        // event triggers StateRefresh (which we don't send here).
        let (tx, rx) = std::sync::mpsc::channel::<DaemonEvent>();
        let mut app = App {
            argv: vec![],
            options: AppOptions {
                config_dir: None,
                data_dir: None,
                engine_dirs: vec![],
                socket_path: None,
                verbosity: 0,
            },
            instance: None,
            state_controller: None,
            ipc_bus: None,
            #[cfg(feature = "systray")]
            tray: None,
            #[cfg(feature = "wayland")]
            frontend: None,
            #[cfg(feature = "wayland")]
            router: None,
            #[cfg(feature = "wayland")]
            repeat_timer: None,
            #[cfg(feature = "wayland")]
            resume_signal: None,
            #[cfg(feature = "wayland")]
            focus_driver: None,
            indicator: None,
            indicator_config: IndicatorConfig::default(),
            #[cfg(feature = "wayland")]
            indicator_timer: None,
            #[cfg(feature = "wayland")]
            indicator_hide_deadline: None,
            config_watcher: None,
            watchdog: None,
            event_tx: tx,
            event_rx: Some(rx),
            saw_restart: false,
        };

        // Empty channel + clear signal flag → no exit.
        assert!(!app.drain_events());
        assert!(!app.saw_restart);

        // Shutdown via channel.
        let _ = app.event_tx.send(DaemonEvent::Shutdown);
        assert!(app.drain_events());
        assert!(!app.saw_restart);

        // Restart sets both saw_restart and should_exit.
        let _ = app.event_tx.send(DaemonEvent::Restart);
        assert!(app.drain_events());
        assert!(app.saw_restart);

        // Signal flag still drives exit (async-signal-safe path).
        app.saw_restart = false;
        signals::SHUTDOWN_FROM_SIGNAL.store(true, Ordering::SeqCst);
        assert!(app.drain_events());
        assert!(!app.saw_restart); // signal path is Shutdown-only

        signals::reset_shutdown_flag();
    }
}
