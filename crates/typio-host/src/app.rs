//! Top-level daemon lifecycle.
//!
//! Port of `src/app.c` + `src/cli.c` + `src/main.c`. Owns the `TypioInstance`,
//! wires engine loading, signal handling, restart-on-exec, and the eventual
//! Wayland frontend / tray / IPC surfaces.

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CString, CStr};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use clap::Parser;
use typio::c_api::registry as c_registry;
use typio::instance::TypioInstance;
use typio::TypioResult;
use typio_abi::TypioEngineInfo;

use crate::config_watcher::ConfigWatcher;
use crate::engine_loader::manifest::EngineManifest;
use crate::engine_loader::resolve_engine_dirs;
use crate::indicator::{EngineModeSnapshot, Indicator, IndicatorConfig, LabelSources, Salience};
use crate::ipc::protocol;
use crate::ipc::protocol::topics;
use crate::ipc_bus::{IpcBus, TypioBackend, TypioRegistryView};
use crate::panel_coordinator::{FlushDecision, UiOwner};
use crate::panel_scheduler::{self, PanelUpdateResult};
use crate::resume_signal::ResumeSignal;
use crate::session_glue::{FocusDriver, FocusTransition};
use crate::state_controller::{StateChange, StateController};
use crate::service::SvcError;
use crate::tray_menu::{EngineDesc, RegistrySnapshot};
use crate::tray_sni::{MenuAction, Tray, TrayAction};
use crate::uds_server::UdsServer;
use crate::watchdog::{LoopStage, Watchdog};

#[cfg(feature = "wayland")]
use {
    nix::sys::time::TimeSpec,
    nix::sys::timerfd::{ClockId, Expiration, TimerFd as NixTimerFd, TimerFlags, TimerSetTimeFlags},
    std::os::fd::{AsFd, AsRawFd},
};

#[cfg(feature = "wayland")]
use crate::input_method::InputMethodFrontend;
#[cfg(feature = "wayland")]
use crate::keyboard::router::{KeyboardRouter, RepeatOutcome};
#[cfg(feature = "wayland")]
use crate::repeat_timer::{self, RepeatTimer};

/// Command-line options for the typio daemon.
#[derive(Parser, Debug, Clone)]
#[command(name = "typio", version, about = "Typio Wayland input-method daemon")]
struct Cli {
    /// Configuration directory.
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Data directory.
    #[arg(short, long)]
    data: Option<PathBuf>,
    /// Engine directory (repeatable; highest precedence).
    #[arg(short = 'E', long)]
    engine_dir: Vec<PathBuf>,
    /// Unix-domain control socket path.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Increase logging verbosity (-v debug, -vv trace).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// Runtime options after CLI parsing and directory resolution.
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

/// Async-signal-safe shutdown flag.
///
/// Only the SIGINT/SIGTERM handler writes this. The main loop translates
/// it into a daemon exit on the next tick. Non-signal paths
/// (`DaemonEvent::Shutdown` via the event channel) must NOT touch this
/// flag — keeping it signal-only preserves async-signal-safety.
static SHUTDOWN_FROM_SIGNAL: AtomicBool = AtomicBool::new(false);

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

extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_FROM_SIGNAL.store(true, Ordering::SeqCst);
}

/// Process-global sender for the mode-changed callback. Stored in a
/// `OnceLock` because the C ABI callback holds a raw `user_data` pointer
/// that must be valid for the instance's lifetime, and there is only one
/// daemon per process. The `Mutex` makes `&Sender` safely shareable
/// across the engine communication thread (where out-of-process engine
/// responses fire the callback) and the main loop thread.
static MODE_CALLBACK_TX: OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<DaemonEvent>>> =
    OnceLock::new();

/// C trampoline for `TypioKeyboardModeChangedCallback`. Fires when an
/// engine reports a **deliberate** mode change (e.g. rime switching schema
/// or toggling 中/A). Marshals to the main loop via `DaemonEvent::StateRefresh`;
/// the main loop then reads the fresh mode from
/// `typio_instance_get_last_keyboard_mode` and triggers the indicator's
/// no-gate deliberate-change path.
///
/// The first parameter uses the **opaque** `typio_abi::TypioInstance`
/// (not `typio::instance::TypioInstance`) to match the callback typedef
/// exactly. The actual pointer is to the real struct; we never dereference
/// it here, so the opacity is harmless.
extern "C" fn mode_changed_trampoline(
    _instance: *mut typio_abi::TypioInstance,
    _mode: *const typio_abi::TypioKeyboardEngineMode,
    _user_data: *mut c_void,
) {
    if let Some(mutex) = MODE_CALLBACK_TX.get() {
        if let Ok(tx) = mutex.lock() {
            let _ = tx.send(DaemonEvent::StateRefresh);
        }
    }
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            signal_handler as *const () as libc::sighandler_t,
        );
    }
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

        // Register engines from the resolved directories via the C ABI.
        let raw = instance.as_mut() as *mut TypioInstance;
        let registry = typio::instance::typio_instance_get_registry(raw);
        if registry.is_null() {
            return Err("engine registry not available".to_string());
        }

        let mut registered_keyboards: Vec<String> = Vec::new();
        let mut registered_voices: Vec<String> = Vec::new();
        for dir in &engine_dirs {
            let dir_path = std::path::Path::new(dir);
            if !dir_path.is_dir() {
                continue;
            }
            let entries = match std::fs::read_dir(dir_path) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !crate::engine_loader::manifest::is_manifest_filename(name) {
                    continue;
                }
                let manifest = match EngineManifest::read_from(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if let Some((engine_name, engine_type)) =
                    register_engine_process(registry, &manifest, &path)
                {
                    if engine_type == "voice" {
                        registered_voices.push(engine_name);
                    } else {
                        registered_keyboards.push(engine_name);
                    }
                }
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
        // The trampoline stores its sender in `MODE_CALLBACK_TX` so the
        // callback (which fires on the engine-comm thread for out-of-process
        // engines like rime) can safely reach the main loop. Without this,
        // only Ctrl+Shift engine switches trigger the indicator — rime's
        // own mode/schema switches are silent.
        let _ = MODE_CALLBACK_TX.set(std::sync::Mutex::new(self.event_tx.clone()));
        {
            let raw = self.instance.as_ref().unwrap().as_ref() as *const TypioInstance as *mut TypioInstance;
            typio::instance::typio_instance_set_keyboard_mode_changed_callback(
                raw,
                mode_changed_trampoline as _,
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

        install_signal_handlers();

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
    fn run_with_wayland(&mut self, ipc_bus: &Rc<RefCell<IpcBus>>) -> i32 {
        let wl_fd = self.frontend.as_mut().unwrap().fd();
        let uds_fd = ipc_bus.borrow().epoll_fd();
        let repeat_fd = self.repeat_timer.as_mut().unwrap().fd();
        // Re-borrow the watchdog field on every use so a long-lived immutable
        // borrow does not block the mutable borrow needed by reload_config().
        macro_rules! wd {
            () => {
                self.watchdog.as_ref().unwrap()
            };
        }

        let (inotify_fd, cfg_timer_fd) = self
            .config_watcher
            .as_ref()
            .map(|w| (w.inotify_fd(), w.timer_fd()))
            .unwrap_or((-1, -1));

        // Indicator auto-hide timer. We pull the raw fd up-front (stable
        // for the timerfd's lifetime) so we can add it to the static poll
        // set; the timer is armed/disarmed via `TimerFd::set` elsewhere.
        let indicator_fd = self
            .indicator_timer
            .as_ref()
            .map(|t| t.as_fd().as_raw_fd())
            .unwrap_or(-1);

        let mut fds = [
            libc::pollfd {
                fd: wl_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: uds_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: repeat_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: inotify_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: cfg_timer_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: indicator_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];

        while !self.drain_events() {
            wd!().set_stage(LoopStage::Idle);
            wd!().heartbeat();

            // 1. Start-of-tick fact bookkeeping.
            {
                let frontend = self.frontend.as_mut().unwrap();
                frontend.state_mut().facts_mut().connection_alive = true;
                if let Some(ref mut rs) = self.resume_signal {
                    if !rs.tick().is_empty() {
                        frontend.state_mut().facts_mut().suspend_gap_detected = true;
                    }
                }
                if frontend.stopped() {
                    eprintln!("typio: input method unavailable");
                    return 1;
                }
            }

            // 2. Flush outgoing Wayland requests, then prepare a read and
            //    dispatch any already-queued events before polling.
            wd!().set_stage(LoopStage::Flush);
            {
                let frontend = self.frontend.as_ref().unwrap();
                if let Err(e) = frontend.flush() {
                    eprintln!("Wayland flush error: {e}");
                    return 1;
                }
            }
            wd!().set_stage(LoopStage::PrepareRead);
            let read_guard = {
                let frontend = self.frontend.as_mut().unwrap();
                match frontend.prepare_read_loop() {
                    Ok(guard) => Some(guard),
                    Err(e) => {
                        eprintln!("Wayland prepare_read error: {e}");
                        return 1;
                    }
                }
            };
            wd!().set_stage(LoopStage::DispatchPending);
            ipc_bus.borrow_mut().dispatch();

            fds[0].revents = 0;
            fds[1].revents = 0;
            fds[2].revents = 0;
            fds[3].revents = 0;
            fds[4].revents = 0;
            fds[5].revents = 0;

            // 3. Poll. Let the panel scheduler and the panel anchor deadline
            //    shorten the timeout.
            wd!().set_stage(LoopStage::Poll);
            wd!().heartbeat();
            let timeout_ms = {
                let frontend = self.frontend.as_ref().unwrap();
                let state = frontend.state();
                let router = self.router.as_ref().unwrap();
                let flushable = panel_scheduler::should_flush(
                    state.panel_schedule_state,
                    router.is_focused(),
                    !router.ctx().is_null(),
                    router.is_focused(),
                );
                let mut timeout_ms =
                    panel_scheduler::poll_timeout_ms(state.panel_schedule_state, flushable, 100);
                if let Some(remaining) = state
                    .panel_coord
                    .anchor_deadline_remaining_ms(Instant::now())
                {
                    timeout_ms = timeout_ms.min(remaining as i32);
                }
                if let Some(indicator_remaining) =
                    self.indicator_hide_remaining_ms(Instant::now())
                {
                    timeout_ms = timeout_ms.min(indicator_remaining);
                }
                timeout_ms
            };
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout_ms) };
            if rc < 0 {
                let e = std::io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                eprintln!("poll error: {e}");
                return 1;
            }

            // 4. Read and dispatch new Wayland events, or cancel the prepared read.
            wd!().set_stage(LoopStage::ReadEvents);
            if fds[0].revents & libc::POLLIN != 0 {
                let frontend = self.frontend.as_mut().unwrap();
                if let Some(guard) = read_guard {
                    if let Err(e) = frontend.read_and_dispatch(guard) {
                        eprintln!("Wayland read error: {e}");
                        return 1;
                    }
                }
            } else if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                eprintln!("Wayland display disconnected");
                return 1;
            }
            // If POLLIN was not set, `read_guard` is dropped here and cancels the read.

            // 5. Run the focus-controller pipeline.
            wd!().set_stage(LoopStage::AuxIo);
            let focus_transition = {
                let engine_present = self
                    .instance
                    .as_ref()
                    .and_then(|i| i.registry_rust())
                    .map(|r| r.active_keyboard_name().is_some())
                    .unwrap_or(false);
                let frontend = self.frontend.as_mut().unwrap();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();
                let mut transition = None;
                if let Some(ref mut driver) = self.focus_driver {
                    transition = driver.tick(frontend, router, timer, engine_present);
                }
                transition
            };

            // 5b. Translate the focus transition into an indicator trigger.
            //    The focus driver has already applied its effects (grab
            //    build/teardown, anchor reset, panel hide on deactivate);
            //    this only layers the indicator on top.
            if let Some(t) = focus_transition {
                match t {
                    FocusTransition::FirstActivate => self.trigger_indicator_focus(),
                    FocusTransition::Reactivate => self.trigger_indicator_reactivate(),
                    FocusTransition::Deactivate => self.hide_indicator(),
                }
            }

            // 6. Drain engine output and process any pending key event.
            {
                let frontend = self.frontend.as_mut().unwrap();
                let state = frontend.state_mut();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();

                router.drain_commit(state);
                router.drain_composition(state);

                if let Some(key) = state.take_pending_key() {
                    // Snapshot the values we need before any mutable borrows
                    // below — both are cheap `Copy` reads.
                    let mods = state.mods_depressed;
                    let compositor_info = state.compositor_repeat_info;
                    if key.state == 1 {
                        let consumed = router.dispatch_key(&key, mods);
                        // Any key that reached the engine (consumed or
                        // forwarded) counts as "user activity" for the
                        // indicator's acknowledged-recency gate. Releases,
                        // modifier-only events, and filtered-out keys do
                        // not (mirrors the C `record_key_activity` caller
                        // in keyboard.c).
                        if let Some(indicator) = self.indicator.as_mut() {
                            indicator.record_key_activity(Instant::now());
                        }
                        if router.take_switch_chord_fired() {
                            // Ctrl+Shift (default) just completed. Cycle
                            // to the next registered keyboard and let the
                            // next drain refresh surfaces. Suppresses
                            // forwarding of the modifier press itself.
                            eprintln!("indicator: Ctrl+Shift chord fired");
                            let instance_ptr = self
                                .instance
                                .as_mut()
                                .map(|i| i.as_mut() as *mut TypioInstance)
                                .unwrap_or(std::ptr::null_mut());
                            cycle_active_keyboard(instance_ptr);
                            let _ = self.event_tx.send(DaemonEvent::StateRefresh);
                        } else if consumed {
                            // Engine consumed the key. Drain any output it
                            // produced, then arm the repeat timer in engine
                            // mode so the held key re-dispatches with
                            // `is_repeat: true` (e.g. backspace deleting a
                            // long preedit one char per tick).
                            router.drain_commit(state);
                            router.drain_composition(state);
                            router.on_consumed(key.clone());
                            arm_repeat(timer, compositor_info, mods);
                        } else {
                            // Engine declined the key; forward it to the
                            // focused app and arm the timer in forward mode
                            // so the main loop synthesises repeats.
                            state.forward_key(key.time, key.keycode, key.state);
                            router.on_forward(key.clone());
                            arm_repeat(timer, compositor_info, mods);
                        }
                    } else {
                        // Forward release events to the engine so
                        // engines that need them (e.g. Rime schema
                        // switching on a lone Shift release) can
                        // complete gesture detection. Modifier state
                        // is mirrored separately via the Modifiers
                        // grab event, so not forwarding a consumed
                        // release here does not leave a stuck modifier
                        // in the focused app.
                        let consumed = router.dispatch_key(&key, mods);
                        if consumed {
                            router.drain_commit(state);
                            router.drain_composition(state);
                        } else {
                            state.forward_key(key.time, key.keycode, key.state);
                        }
                        router.on_release(&key);
                        let _ = timer.stop();
                    }
                }
            }

            // 7. Flush the candidate panel if the scheduler says so.
            wd!().set_stage(LoopStage::PanelUpdate);
            {
                let frontend = self.frontend.as_mut().unwrap();
                let router = self.router.as_mut().unwrap();
                let (schedule_state, candidates, selected) = {
                    let state = frontend.state();
                    (
                        state.panel_schedule_state,
                        state.candidates.clone(),
                        state.selected_candidate,
                    )
                };
                let has_session = router.is_focused();
                let has_context = !router.ctx().is_null();
                let context_focused = router.is_focused();
                if panel_scheduler::should_flush(
                    schedule_state,
                    has_session,
                    has_context,
                    context_focused,
                ) {
                    let scale = frontend.state().buffer_scale;
                    let result = if let Some(panel) = frontend.panel_mut() {
                        panel.set_scale(scale);
                        if candidates.is_empty() {
                            eprintln!("panel: hide (no candidates)");
                            panel.hide();
                        } else {
                            panel.ensure_candidate_size(&candidates);
                            eprintln!(
                                "panel: draw {} candidate(s), selected={}",
                                candidates.len(),
                                selected
                            );
                            panel.draw_candidates(&candidates, selected);
                        }
                        PanelUpdateResult::Done
                    } else {
                        eprintln!("panel: flush requested but no FluxPanel attached");
                        PanelUpdateResult::Done
                    };
                    frontend.state_mut().panel_schedule_state = panel_scheduler::complete(result);
                }
            }

            // 7b. Flush any pending positioned status UI (indicator / voice)
            //     when the anchor becomes ready or the caret fallback fires.
            //     Drives the deferred-show path: a `show_on_focus` or
            //     `show_for_state_change` call returned a label, the
            //     coordinator queued it because the anchor wasn't ready,
            //     and now the anchor resolved (or the caret fallback fired).
            {
                let now = Instant::now();
                let flushed = {
                    let frontend = self.frontend.as_mut().unwrap();
                    let state = frontend.state_mut();
                    state
                        .panel_coord_mut()
                        .flush_pending_with_timeout(now)
                };
                if let Some((owner, label)) = flushed {
                    eprintln!(
                        "indicator: deferred flush owner={:?} label='{}'",
                        owner, label
                    );
                    if owner == UiOwner::Indicator {
                        self.render_indicator_banner(&label, now);
                    }
                }
                // UiOwner::Voice is reserved for a future chunk; the flush
                // path is in place and tested by panel_coordinator's queue
                // tests, but no producer feeds it yet.
            }

            // 8. Repeat timer expiration.
            if fds[2].revents & libc::POLLIN != 0 {
                wd!().set_stage(LoopStage::Repeat);
                let mut buf = [0u8; 8];
                unsafe {
                    libc::read(repeat_fd, buf.as_mut_ptr() as *mut c_void, buf.len());
                }
                let frontend = self.frontend.as_mut().unwrap();
                let state = frontend.state_mut();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();
                let mods = state.mods_depressed;
                match router.dispatch_repeat(state, mods) {
                    RepeatOutcome::Forwarded => {}
                    RepeatOutcome::Consumed => {
                        router.drain_commit(state);
                        router.drain_composition(state);
                    }
                    RepeatOutcome::Stopped => {
                        let _ = timer.stop();
                    }
                }
            }

            // 8b. Indicator auto-hide timer expiration. The timerfd fires
            //     once after `display.indicator_duration_ms`; we hide the
            //     popup and disarm. The indicator's recency tracking is
            //     left intact so a recent indicator still suppresses the
            //     next focus-path reveal.
            if fds[5].revents & libc::POLLIN != 0 {
                let mut buf = [0u8; 8];
                if let Some(tf) = self.indicator_timer.as_ref() {
                    unsafe {
                        libc::read(
                            tf.as_fd().as_raw_fd(),
                            buf.as_mut_ptr() as *mut c_void,
                            buf.len(),
                        );
                    }
                }
                self.hide_indicator();
            }

            // End-of-tick heartbeat.
            wd!().stage_done();

            // 9. Config watcher events. These are handled after the main
            //    pipeline so a temporary field borrow can be used for the
            //    config reload without colliding with the watchdog macro.
            if fds[3].revents & libc::POLLIN != 0 {
                if let Some(ref mut watcher) = self.config_watcher {
                    match watcher.drain_inotify() {
                        Ok(outcome) => {
                            if outcome.should_rearm_watches {
                                let _ = watcher.rearm_watches();
                            }
                            if outcome.should_schedule_reload {
                                let _ = watcher.schedule_reload();
                            }
                        }
                        Err(e) => eprintln!("config watcher inotify error: {e}"),
                    }
                }
            }
            if fds[4].revents & libc::POLLIN != 0 {
                let should_reload = if let Some(ref mut watcher) = self.config_watcher {
                    match watcher.drain_timer() {
                        Ok(true) => true,
                        Ok(false) => false,
                        Err(e) => {
                            eprintln!("config watcher timer error: {e}");
                            false
                        }
                    }
                } else {
                    false
                };
                if should_reload {
                    wd!().set_stage(LoopStage::ConfigReload);
                    self.reload_config();
                }
            }
        }

        eprintln!("typio: shutting down...");
        0
    }

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

    /// Read the indicator configuration snapshot from libtypio. The keys
    /// (`display.indicator_enabled`, `display.indicator_duration_ms`) are
    /// read on demand from the live config object, matching the C host's
    /// behaviour, but the values are cached on [`App`] so the hot path
    /// never crosses the FFI.
    fn load_indicator_config(&self) -> IndicatorConfig {
        let raw = match self.instance.as_ref() {
            Some(i) => i.as_ref() as *const TypioInstance as *mut TypioInstance,
            None => return IndicatorConfig::default(),
        };
        let cfg = typio::instance::typio_instance_get_config(raw);
        if cfg.is_null() {
            return IndicatorConfig::default();
        }
        let enabled =
            typio::config::typio_config_get_bool(cfg, c"display.indicator_enabled".as_ptr(), true);
        let duration_ms = typio::config::typio_config_get_int(
            cfg,
            c"display.indicator_duration_ms".as_ptr(),
            1500,
        );
        IndicatorConfig::from_values(enabled, duration_ms.into())
    }

    /// Trigger the indicator's focus-path show (FirstActivate). Reads the
    /// live registry and cached mode from libtypio; applies the salience
    /// gate + acknowledged-recency gate.
    #[cfg(feature = "wayland")]
    fn trigger_indicator_focus(&mut self) {
        self.trigger_indicator_show(IndicatorPath::Focus);
    }

    /// Trigger the indicator's reactivate-path show (Reactivate). Gates:
    /// salience only — recency is skipped per ADR-0018.
    #[cfg(feature = "wayland")]
    fn trigger_indicator_reactivate(&mut self) {
        self.trigger_indicator_show(IndicatorPath::Reactivate);
    }

    /// Trigger the indicator's deliberate-change show (no gates beyond
    /// `enabled`). Called from the `StateRefresh` drain — covers Ctrl+Shift
    /// chord, tray-driven engine/language switch, and IPC-driven mutations.
    #[cfg(feature = "wayland")]
    fn trigger_indicator_state_change(&mut self) {
        self.trigger_indicator_show(IndicatorPath::StateChange);
    }

    /// Shared body of the three trigger paths. Resolves label sources from
    /// the live registry, asks the [`Indicator`] state machine for a label,
    /// and feeds any returned label to [`Self::request_indicator_show`].
    #[cfg(feature = "wayland")]
    fn trigger_indicator_show(&mut self, path: IndicatorPath) {
        let now = Instant::now();

        // Read the current mode from libtypio's cache. The mode-changed
        // callback stores the fresh mode in `last_mode` before firing, so
        // by the time StateRefresh delivers us here, the data is current.
        // This covers rime's schema/mode switches: the engine reports its
        // new active mode (e.g. display_label="中", salience=Notable), the
        // callback fires, we read it here, and the indicator shows the mode
        // suffix instead of just the bare engine name.
        let mode_display: Option<String> = self.read_mode_display_label();
        let mode_salience = self.read_mode_salience();

        let label = {
            let Some(instance) = self.instance.as_ref() else {
                eprintln!("indicator: no instance, skipping");
                return;
            };
            let Some(registry) = instance.registry_rust() else {
                eprintln!("indicator: no registry, skipping");
                return;
            };
            let sources = RegistryLabelSources { registry };
            let Some(indicator) = self.indicator.as_mut() else {
                eprintln!("indicator: no indicator state machine, skipping");
                return;
            };
            let cfg = self.indicator_config;

            // Build the mode snapshot from the libtypio-cached mode. The
            // snapshot always exists when we have a valid mode pointer,
            // even if `display_label` is None — the salience gate still
            // needs to see it.
            let mode_snapshot = EngineModeSnapshot {
                display_label: mode_display.as_deref(),
                salience: mode_salience,
            };
            let mode_ref = Some(&mode_snapshot);

            let label = match path {
                IndicatorPath::Focus => {
                    indicator.show_on_focus(now, mode_ref, &cfg, &sources)
                }
                IndicatorPath::Reactivate => {
                    indicator.show_on_reactivate(now, mode_ref, &cfg, &sources)
                }
                IndicatorPath::StateChange => {
                    indicator.show_for_state_change(now, mode_ref, &cfg, &sources)
                }
            };
            eprintln!(
                "indicator: path={:?} mode_display={:?} salience={:?} lang={:?} engine={:?} → label={:?}",
                path,
                mode_display,
                mode_salience,
                sources.active_language_tag(),
                sources.active_engine_name(),
                label
            );
            label
        };
        if let Some(label) = label {
            self.request_indicator_show(label, now);
        }
    }

    /// Read the cached mode's `display_label` from libtypio (e.g. "中",
    /// "A", "Latin"). Returns `None` when no engine has reported a mode
    /// yet, or when the mode has no display label.
    fn read_mode_display_label(&self) -> Option<String> {
        let raw = self.instance.as_ref()?;
        let raw = raw.as_ref() as *const TypioInstance as *mut TypioInstance;
        let mode_ptr = typio::instance::typio_instance_get_last_keyboard_mode(raw);
        if mode_ptr.is_null() {
            return None;
        }
        let mode = unsafe { &*mode_ptr };
        if mode.display_label.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(mode.display_label) }.to_string_lossy().into_owned())
        }
    }

    /// Read the cached mode's salience. Returns `Quiet` when no mode is set.
    fn read_mode_salience(&self) -> Salience {
        let raw = match self.instance.as_ref() {
            Some(i) => i.as_ref() as *const TypioInstance as *mut TypioInstance,
            None => return Salience::Quiet,
        };
        let mode_ptr = typio::instance::typio_instance_get_last_keyboard_mode(raw);
        if mode_ptr.is_null() {
            return crate::indicator::Salience::Quiet;
        }
        let mode = unsafe { &*mode_ptr };
        match mode.salience {
            typio_abi::TypioStatusSalience::TypioStatusSalienceNotable => Salience::Notable,
            _ => Salience::Quiet,
        }
    }

    /// Feed an indicator show request through the [`PanelCoordinator`].
    /// If the anchor is ready the banner renders immediately and the
    /// auto-hide timer is armed; otherwise the coordinator queues the
    /// request and it flushes on a later tick through
    /// `flush_pending_with_timeout`.
    #[cfg(feature = "wayland")]
    fn request_indicator_show(&mut self, label: String, now: Instant) {
        let decision = {
            let Some(frontend) = self.frontend.as_mut() else {
                eprintln!("indicator: no frontend, skipping");
                return;
            };
            let coord = frontend.state_mut().panel_coord_mut();
            let anchor_ready = coord.anchor_ready();
            let decision = coord.decide_positioned_flush(UiOwner::Indicator, &label);
            eprintln!(
                "indicator: coordinator anchor_ready={} → {:?}",
                anchor_ready, decision
            );
            decision
        };
        match decision {
            FlushDecision::Show => self.render_indicator_banner(&label, now),
            FlushDecision::Pending => {
                eprintln!("indicator: queued, will flush when anchor resolves");
            }
            FlushDecision::Cancel => {
                eprintln!("indicator: coordinator cancelled the request");
                if let Some(indicator) = self.indicator.as_mut() {
                    indicator.hide();
                }
            }
        }
    }

    /// Render the indicator banner onto the candidate Panel's surface,
    /// mark the indicator as shown (updates recency tracking), and arm
    /// the auto-hide timer. Called either from
    /// [`Self::request_indicator_show`] when the coordinator accepts
    /// immediately, or from the loop's `flush_pending_with_timeout` path
    /// when a queued show later becomes renderable.
    #[cfg(feature = "wayland")]
    fn render_indicator_banner(&mut self, label: &str, now: Instant) {
        let t0 = Instant::now();
        eprintln!("indicator: rendering banner '{label}'");
        let scale = self
            .frontend
            .as_ref()
            .map(|f| f.state().buffer_scale)
            .unwrap_or(1.0);
        if let Some(panel) = self
            .frontend
            .as_mut()
            .and_then(|f| f.panel_mut())
        {
            panel.set_scale(scale);
            let t1 = Instant::now();
            eprintln!("indicator: set_scale={} took {:.3} ms", scale, t1.duration_since(t0).as_secs_f64() * 1000.0);
            panel.ensure_banner_size(label);
            let t2 = Instant::now();
            eprintln!("indicator: ensure_banner_size took {:.3} ms", t2.duration_since(t1).as_secs_f64() * 1000.0);
            panel.draw_status_banner(label);
            let t3 = Instant::now();
            eprintln!("indicator: draw_status_banner took {:.3} ms (banner rendered and presented)", t3.duration_since(t2).as_secs_f64() * 1000.0);
        } else {
            eprintln!("indicator: no FluxPanel attached, cannot render");
        }
        if let Some(indicator) = self.indicator.as_mut() {
            indicator.note_shown(now);
        }
        self.arm_indicator_timer(now);
    }

    /// Hide the indicator (timer expiry, deactivate, or coordinator
    /// cancel). Clears the indicator's active flag, dismisses any queued
    /// request for the Indicator owner, detaches the popup surface if the
    /// Indicator was the visible owner, and disarms the auto-hide timer.
    /// Leaves the recency edges intact: a recently-shown indicator still
    /// suppresses the next focus-path reveal.
    #[cfg(feature = "wayland")]
    fn hide_indicator(&mut self) {
        if let Some(indicator) = self.indicator.as_mut() {
            indicator.hide();
        }
        let indicator_owned = {
            let Some(frontend) = self.frontend.as_mut() else {
                return;
            };
            let coord = frontend.state_mut().panel_coord_mut();
            let was_visible = coord.visible_owner() == UiOwner::Indicator;
            coord.hide(UiOwner::Indicator);
            was_visible
        };
        if indicator_owned {
            if let Some(panel) = self
                .frontend
                .as_mut()
                .and_then(|f| f.panel_mut())
            {
                panel.hide();
            }
        }
        self.disarm_indicator_timer();
    }

    /// Arm the auto-hide timer for the indicator's configured duration
    /// (clamped to 100–10000 ms in [`IndicatorConfig`]). Idempotent —
    /// re-arming replaces any prior deadline.
    #[cfg(feature = "wayland")]
    fn arm_indicator_timer(&mut self, now: Instant) {
        let duration = self.indicator_config.duration;
        if let Some(tf) = self.indicator_timer.as_ref() {
            let expiration =
                Expiration::OneShot(TimeSpec::from_duration(duration));
            let _ = tf.set(expiration, TimerSetTimeFlags::empty());
        }
        self.indicator_hide_deadline = Some(now + duration);
    }

    /// Disarm the auto-hide timer. Safe to call on an already-disarmed
    /// timer; arming with a zero `it_value` is the kernel-defined disarm.
    #[cfg(feature = "wayland")]
    fn disarm_indicator_timer(&mut self) {
        if let Some(tf) = self.indicator_timer.as_ref() {
            let expiration =
                Expiration::OneShot(TimeSpec::from_duration(std::time::Duration::ZERO));
            let _ = tf.set(expiration, TimerSetTimeFlags::empty());
        }
        self.indicator_hide_deadline = None;
    }

    /// Remaining milliseconds until the indicator auto-hide deadline, or
    /// `None` when the timer is not armed.
    #[cfg(feature = "wayland")]
    fn indicator_hide_remaining_ms(&self, now: Instant) -> Option<i32> {
        self.indicator_hide_deadline
            .and_then(|d| d.checked_duration_since(now))
            .map(|rem| rem.as_millis() as i32)
            .map(|ms| ms.max(0))
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
        let mut should_exit = SHUTDOWN_FROM_SIGNAL.swap(false, Ordering::Relaxed);
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

#[cfg(feature = "systray")]
fn install_tray_action_handler(
    tray: &Tray,
    instance: *mut TypioInstance,
    event_tx: std::sync::mpsc::Sender<DaemonEvent>,
) {
    // Cast to usize so the closure is Send; reconstruct inside each arm.
    let instance_ptr = instance as usize;
    tray.set_action_handler(move |action| {
        let instance = instance_ptr as *mut TypioInstance;
        let event = match action {
            TrayAction::Menu(MenuAction::Restart) => Some(DaemonEvent::Restart),
            TrayAction::Menu(MenuAction::Quit) => Some(DaemonEvent::Shutdown),
            TrayAction::Menu(MenuAction::Language(idx)) => {
                language_at_index(instance, idx as usize)
                    .and_then(|tag| set_active_language(instance, &tag).ok())
                    .map(|_| DaemonEvent::StateRefresh)
            }
            TrayAction::Menu(MenuAction::EngineInLanguage {
                lang_idx: _,
                engine_idx,
            }) => keyboard_at_index(instance, engine_idx as usize)
                .and_then(|name| set_active_keyboard(instance, &name).ok())
                .map(|_| DaemonEvent::StateRefresh),
            TrayAction::Menu(MenuAction::OrphanEngine(idx)) => {
                orphan_keyboard_at_index(instance, idx as usize)
                    .and_then(|name| set_active_keyboard(instance, &name).ok())
                    .map(|_| DaemonEvent::StateRefresh)
            }
            TrayAction::Menu(MenuAction::Voice(idx)) => voice_at_index(instance, idx as usize)
                .and_then(|name| set_active_voice(instance, &name).ok())
                .map(|_| DaemonEvent::StateRefresh),
            _ => None,
        };
        if let Some(event) = event {
            let _ = event_tx.send(event);
        }
    });
}

#[cfg(feature = "systray")]
fn update_tray_from_controller(
    tray: &Tray,
    controller: &StateController<TypioRegistryView>,
    instance: *mut TypioInstance,
) {
    tray.update_engine(controller.active_engine_name(), controller.engine_active());
    if controller.status_icon_is_badge() {
        tray.set_badge(controller.status_badge_text());
    } else {
        tray.set_icon(Some(controller.status_icon()));
    }
    if let Some(snapshot) = build_tray_snapshot(instance) {
        tray.set_menu_snapshot(snapshot);
    }
}

fn build_tray_snapshot(instance: *mut TypioInstance) -> Option<RegistrySnapshot> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    let languages = reg.known_languages();
    let mut keyboards = Vec::new();
    for name in reg.list_keyboards() {
        let info = reg.engine_info(name)?;
        keyboards.push(EngineDesc {
            name: name.to_string(),
            display_name: Some(info.display_name.clone()),
            languages: info
                .effective_languages()
                .iter()
                .map(|s| s.to_string())
                .collect(),
        });
    }
    let mut voices = Vec::new();
    for name in reg.list_voices() {
        let info = reg.engine_info(name)?;
        voices.push(EngineDesc {
            name: name.to_string(),
            display_name: Some(info.display_name.clone()),
            languages: Vec::new(),
        });
    }
    Some(RegistrySnapshot {
        languages,
        active_language: reg.active_language().map(str::to_string),
        keyboards,
        voices,
        active_voice: reg.active_voice_name().map(str::to_string),
    })
}

fn language_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    reg.known_languages().get(idx).cloned()
}

fn keyboard_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    reg.list_keyboards().get(idx).map(|n| n.to_string())
}

fn orphan_keyboard_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    let known: std::collections::HashSet<String> = reg.known_languages().into_iter().collect();
    let orphans: Vec<String> = reg
        .list_keyboards()
        .into_iter()
        .filter(|name| {
            reg.engine_info(name)
                .map(|info| {
                    info.effective_languages()
                        .iter()
                        .all(|l| !known.contains(l))
                })
                .unwrap_or(true)
        })
        .map(str::to_string)
        .collect();
    orphans.get(idx).cloned()
}

fn voice_at_index(instance: *mut TypioInstance, idx: usize) -> Option<String> {
    let inst = unsafe { instance.as_ref() }?;
    let reg = inst.registry_rust()?;
    reg.list_voices().get(idx).map(|n| n.to_string())
}

fn set_active_language(instance: *mut TypioInstance, tag: &str) -> Result<(), SvcError> {
    let reg = registry_ptr(instance).ok_or(SvcError)?;
    let tag_c = CString::new(tag).map_err(|_| SvcError)?;
    match c_registry::typio_registry_set_active_language(reg, tag_c.as_ptr()) {
        typio::TypioResult::TypioOk => Ok(()),
        _ => Err(SvcError),
    }
}

fn set_active_keyboard(instance: *mut TypioInstance, name: &str) -> Result<(), SvcError> {
    let reg = registry_ptr(instance).ok_or(SvcError)?;
    let name_c = CString::new(name).map_err(|_| SvcError)?;
    match c_registry::typio_registry_set_active_keyboard(reg, name_c.as_ptr()) {
        typio::TypioResult::TypioOk => {
            eprintln!("tray: active keyboard -> {name}");
            Ok(())
        }
        _ => {
            eprintln!("tray: set_active_keyboard({name}) failed");
            Err(SvcError)
        }
    }
}

/// Cycle to the next registered keyboard engine, called when the user
/// presses the Ctrl+Shift engine-switch chord. Wraps from last back to
/// first; if only one keyboard is registered, the call is a no-op.
fn cycle_active_keyboard(instance: *mut TypioInstance) {
    let Some(inst) = (unsafe { instance.as_ref() }) else {
        return;
    };
    let Some(reg) = inst.registry_rust() else {
        return;
    };
    let keyboards: Vec<&str> = reg.list_keyboards();
    if keyboards.len() < 2 {
        return;
    }
    let current: &str = reg.active_keyboard_name().unwrap_or(keyboards[0]);
    let next = keyboards
        .iter()
        .position(|k| *k == current)
        .and_then(|i| keyboards.get((i + 1) % keyboards.len()).copied())
        .unwrap_or(keyboards[0]);
    let _ = set_active_keyboard(instance, next);
}

fn set_active_voice(instance: *mut TypioInstance, name: &str) -> Result<(), SvcError> {
    let reg = registry_ptr(instance).ok_or(SvcError)?;
    let name_c = CString::new(name).map_err(|_| SvcError)?;
    match c_registry::typio_registry_set_active_voice(reg, name_c.as_ptr()) {
        typio::TypioResult::TypioOk => Ok(()),
        _ => Err(SvcError),
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
/// Which indicator show-path to take. Mirror of [`Indicator`]'s three
/// public methods, lifted into a tag so [`App::trigger_indicator_show`]
/// can dispatch on a single borrow scope without re-borrowing `self`
/// for each arm.
#[cfg(feature = "wayland")]
#[derive(Debug, Clone, Copy)]
enum IndicatorPath {
    Focus,
    Reactivate,
    StateChange,
}

/// [`LabelSources`] backed by the live `EngineRegistry`. Borrows its
/// strings so the indicator label composition is zero-allocation on the
/// hot path.
#[cfg(feature = "wayland")]
struct RegistryLabelSources<'a> {
    registry: &'a typio::core::registry::EngineRegistry,
}

#[cfg(feature = "wayland")]
impl<'a> LabelSources for RegistryLabelSources<'a> {
    fn active_language_tag(&self) -> Option<&str> {
        self.registry.active_language()
    }
    fn active_engine_name(&self) -> Option<&str> {
        self.registry.active_keyboard_name()
    }
    fn active_engine_display_name(&self) -> Option<&str> {
        self.registry
            .active_keyboard_name()
            .and_then(|name| self.registry.engine_info(name))
            .map(|info| info.display_name.as_str())
    }
}

#[cfg(feature = "wayland")]
fn arm_repeat(
    timer: &mut RepeatTimer,
    compositor_info: Option<(i32, i32)>,
    mods_depressed: u32,
) {
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

fn registry_ptr(
    instance: *mut TypioInstance,
) -> Option<*mut typio::c_api::registry::TypioRegistry> {
    if instance.is_null() {
        return None;
    }
    let reg = typio::instance::typio_instance_get_registry(instance);
    if reg.is_null() {
        None
    } else {
        Some(reg)
    }
}

/// Register one engine from a manifest via the C ABI.
/// Returns `(name, engine_type)` on success.
fn register_engine_process(
    registry: *mut typio::c_api::registry::TypioRegistry,
    manifest: &EngineManifest,
    path: &std::path::Path,
) -> Option<(String, String)> {
    let c_name = CString::new(manifest.name.as_str()).ok()?;
    let c_display =
        CString::new(manifest.display_name.as_deref().unwrap_or(&manifest.name)).ok()?;
    let c_desc = CString::new(manifest.description.as_deref().unwrap_or("")).ok()?;
    let c_author = CString::new(manifest.author.as_deref().unwrap_or("")).ok()?;
    let c_icon = manifest
        .icon
        .as_ref()
        .and_then(|s| CString::new(s.as_str()).ok());
    let c_lang = CString::new(manifest.primary_language()).ok()?;

    let argv_strings: Vec<CString> = manifest
        .argv(path)
        .ok()?
        .into_iter()
        .filter_map(|s| CString::new(s).ok())
        .collect();
    if argv_strings.is_empty() {
        return None;
    }
    let argv_ptrs: Vec<*const c_char> = argv_strings
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let info = TypioEngineInfo {
        name: c_name.as_ptr(),
        display_name: c_display.as_ptr(),
        description: c_desc.as_ptr(),
        author: c_author.as_ptr(),
        icon: c_icon
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(std::ptr::null()),
        language: c_lang.as_ptr(),
        type_: if manifest.engine_type == "voice" {
            typio_abi::TypioEngineType::TypioEngineTypeVoice
        } else {
            typio_abi::TypioEngineType::TypioEngineTypeKeyboard
        },
        required_capabilities: std::ptr::null(),
        optional_capabilities: std::ptr::null(),
    };

    let result =
        c_registry::typio_registry_register_engine_process(registry, &info, argv_ptrs.as_ptr());

    if result == TypioResult::TypioOk {
        eprintln!(
            "OK:   registered engine '{}' ({}) from {}",
            manifest.name,
            manifest.engine_type,
            path.display()
        );
        let name = manifest.name.clone();
        let engine_type = manifest.engine_type.clone();
        Some((name, engine_type))
    } else {
        eprintln!(
            "WARN: failed to register engine '{}' from {} (result={result:?})",
            manifest.name,
            path.display()
        );
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises tests that touch the shared signal flags so they do not
    /// race with each other when `cargo test` runs them in parallel.
    static SIGNAL_FLAG_LOCK: Mutex<()> = Mutex::new(());

    fn fixture_manifest() -> EngineManifest {
        EngineManifest {
            name: "fixture".to_string(),
            engine_type: "keyboard".to_string(),
            protocol: "typio-engine-protocol".to_string(),
            command: Some("/bin/true".to_string()),
            display_name: Some("Fixture".to_string()),
            description: Some("test fixture".to_string()),
            author: Some("test".to_string()),
            icon: Some("fixture-icon".to_string()),
            language: None,
            languages: Some(vec!["und".to_string()]),
            arg: None,
            args: None,
            required: None,
            optional: None,
        }
    }

    #[test]
    fn cli_parses_into_app_options() {
        let cli = Cli::parse_from([
            "typio", "-c", "/cfg", "--socket", "/sock", "-E", "/e1", "-E", "/e2", "-vv",
        ]);
        let opts: AppOptions = cli.into();
        assert_eq!(opts.config_dir, Some("/cfg".to_string()));
        assert_eq!(opts.data_dir, None);
        assert_eq!(opts.engine_dirs, vec!["/e1".to_string(), "/e2".to_string()]);
        assert_eq!(opts.socket_path, Some(PathBuf::from("/sock")));
        assert_eq!(opts.verbosity, 2);
    }

    #[test]
    fn daemon_events_drive_drain_results() {
        let _guard = SIGNAL_FLAG_LOCK.lock().unwrap();

        // Reset the signal flag so prior tests don't leak.
        SHUTDOWN_FROM_SIGNAL.store(false, Ordering::SeqCst);

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
        SHUTDOWN_FROM_SIGNAL.store(true, Ordering::SeqCst);
        assert!(app.drain_events());
        assert!(!app.saw_restart); // signal path is Shutdown-only

        SHUTDOWN_FROM_SIGNAL.store(false, Ordering::SeqCst);
    }

    #[test]
    fn register_engine_process_round_trip() {
        let inst = typio::instance::typio_instance_new();
        assert!(!inst.is_null());
        assert_eq!(
            typio::instance::typio_instance_init(inst),
            typio::TypioResult::TypioOk
        );

        let reg = typio::instance::typio_instance_get_registry(inst);
        assert!(!reg.is_null());

        let manifest = fixture_manifest();
        let result = register_engine_process(reg, &manifest, std::path::Path::new("/tmp"));
        assert_eq!(
            result,
            Some(("fixture".to_string(), "keyboard".to_string()))
        );

        let snapshot = build_tray_snapshot(inst).expect("tray snapshot should build");
        assert_eq!(snapshot.keyboards.len(), 1);
        assert_eq!(snapshot.keyboards[0].name, "fixture");
        assert_eq!(
            snapshot.keyboards[0].display_name,
            Some("Fixture".to_string())
        );
        assert_eq!(snapshot.voices.len(), 0);

        typio::instance::typio_instance_free(inst);
    }
}
