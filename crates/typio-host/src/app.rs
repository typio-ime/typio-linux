//! Top-level daemon lifecycle.
//!
//! Port of `src/app.c` + `src/cli.c` + `src/main.c`. Owns the `TypioInstance`,
//! wires engine loading, signal handling, restart-on-exec, and the eventual
//! Wayland frontend / tray / IPC surfaces.

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CString};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use typio::c_api::registry as c_registry;
use typio::instance::TypioInstance;
use typio::TypioResult;
use typio_abi::TypioEngineInfo;

use crate::engine_loader::manifest::EngineManifest;
use crate::engine_loader::resolve_engine_dirs;
use crate::ipc::protocol::topics;
use crate::ipc::protocol;
use crate::ipc_bus::{IpcBus, TypioBackend, TypioRegistryView};
use crate::panel_scheduler::{self, PanelUpdateResult};
use crate::resume_signal::ResumeSignal;
use crate::session_glue::FocusDriver;
use crate::state_controller::{StateChange, StateController};
use crate::tray_menu::{EngineDesc, RegistrySnapshot};
use crate::tray_sni::{MenuAction, Tray, TrayAction};
use crate::uds_server::UdsServer;

#[cfg(feature = "wayland")]
use crate::input_method::InputMethodFrontend;
#[cfg(feature = "wayland")]
use crate::keyboard::router::KeyboardRouter;
#[cfg(feature = "wayland")]
use crate::repeat_timer::RepeatTimer;

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

/// Shared, signal-safe shutdown flag.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Set by the tray "Restart" action; read in [`App::finish`] to exec-restart.
static RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT, signal_handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, signal_handler as *const () as libc::sighandler_t);
    }
}

fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

fn restart_requested_flag() -> bool {
    RESTART_REQUESTED.load(Ordering::Relaxed)
}

fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn request_restart() {
    RESTART_REQUESTED.store(true, Ordering::SeqCst);
    request_shutdown();
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
    restart_requested: bool,
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
            restart_requested: false,
        })
    }

    /// Initialize the Typio instance and load engines.
    pub fn init(&mut self) -> Result<(), String> {
        // Engine search path: CLI dirs > $TYPIO_ENGINE_PATH > system dir.
        let engine_dirs = resolve_engine_dirs(self.options.engine_dirs.iter().cloned());

        let mut instance = TypioInstance::new_rust(
            self.options.config_dir.as_deref(),
            self.options.data_dir.as_deref(),
            None, // state_dir — let libtypio pick the default.
            engine_dirs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
        );

        instance
            .init_rust()
            .map_err(|e| format!("TypioInstance init failed: {e:?}"))?;

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
                if !name.starts_with("typio-engine-") || !name.ends_with(".toml") {
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

        // Activate first keyboard engine if any were registered.
        if let Some(first) = registered_keyboards.first() {
            let c_name = CString::new(first.as_str()).unwrap();
            c_registry::typio_registry_set_active_keyboard(registry, c_name.as_ptr());
        }

        self.instance = Some(instance);

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
            }
        }

        let raw = self.instance.as_mut().unwrap().as_mut() as *mut TypioInstance;
        self.state_controller = Some(StateController::new(TypioRegistryView::new(raw)));

        #[cfg(feature = "systray")]
        {
            let mut tray = Tray::new();
            tray.register();
            install_tray_action_handler(&tray, raw);
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
        ipc_bus.borrow_mut().set_stop_callback(request_shutdown);

        if let Some(ref mut controller) = self.state_controller {
            let ipc = ipc_bus.clone();
            controller.add_listener(Box::new(move |change| {
                let (topic, payload) = match change {
                    StateChange::Engine | StateChange::VoiceEngine => {
                        (topics::ENGINE_CHANGED, serde_json::json!({}))
                    }
                    StateChange::Language => {
                        (topics::LANGUAGE_CHANGED, serde_json::json!({}))
                    }
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
            return self.run_with_wayland(&ipc_bus);
        }

        self.run_with_uds(&ipc_bus)
    }

    fn run_with_uds(&self, ipc_bus: &Rc<RefCell<IpcBus>>) -> i32 {
        let uds_fd = ipc_bus.borrow().epoll_fd();
        let mut pollfd = libc::pollfd {
            fd: uds_fd,
            events: libc::POLLIN,
            revents: 0,
        };

        while !shutdown_requested() {
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
        let frontend = self.frontend.as_mut().unwrap();
        let wl_fd = frontend.fd();
        let uds_fd = ipc_bus.borrow().epoll_fd();
        let timer = self.repeat_timer.as_mut().unwrap();
        let repeat_fd = timer.fd();
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
        ];

        let default_delay = crate::repeat_timer::DEFAULT_DELAY;
        let default_interval = RepeatTimer::interval_from_rate(30);

        while !shutdown_requested() {
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

            // 2. Flush and dispatch Wayland events.
            {
                let frontend = self.frontend.as_mut().unwrap();
                if let Err(e) = frontend.flush() {
                    eprintln!("Wayland flush error: {e}");
                    return 1;
                }
                if let Err(e) = frontend.dispatch() {
                    eprintln!("Wayland dispatch error: {e}");
                    return 1;
                }
            }
            ipc_bus.borrow_mut().dispatch();

            // 3. Run the focus-controller pipeline.
            {
                let engine_present = self
                    .instance
                    .as_ref()
                    .and_then(|i| i.registry_rust())
                    .map(|r| r.active_keyboard_name().is_some())
                    .unwrap_or(false);
                let frontend = self.frontend.as_mut().unwrap();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();
                if let Some(ref mut driver) = self.focus_driver {
                    driver.tick(frontend, router, timer, engine_present);
                }
            }

            // 4. Drain engine output and process any pending key event.
            {
                let frontend = self.frontend.as_mut().unwrap();
                let state = frontend.state_mut();
                let router = self.router.as_mut().unwrap();
                let timer = self.repeat_timer.as_mut().unwrap();

                router.drain_commit(state);
                router.drain_composition(state);

                if let Some(key) = state.take_pending_key() {
                    if key.state == 1 {
                        let consumed = router.dispatch_key(&key, state.mods_depressed);
                        if consumed {
                            router.drain_commit(state);
                            router.drain_composition(state);
                            let _ = timer.stop();
                        } else {
                            state.forward_key(key.time, key.keycode, key.state);
                            router.on_forward(key.clone());
                            if crate::repeat_timer::should_repeat_for_modifiers(
                                crate::repeat_timer::Modifiers(state.mods_depressed),
                            ) {
                                let _ = timer.start(default_delay, default_interval);
                            }
                        }
                    } else {
                        state.forward_key(key.time, key.keycode, key.state);
                        router.on_release(&key);
                        let _ = timer.stop();
                    }
                }
            }

            // 5. Flush the candidate panel if the scheduler says so.
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
                    let result = if let Some(panel) = frontend.panel_mut() {
                        if candidates.is_empty() {
                            panel.hide();
                        } else {
                            panel.draw_candidates(&candidates, selected);
                        }
                        PanelUpdateResult::Done
                    } else {
                        PanelUpdateResult::Done
                    };
                    frontend.state_mut().panel_schedule_state = panel_scheduler::complete(result);
                }
            }

            fds[0].revents = 0;
            fds[1].revents = 0;
            fds[2].revents = 0;

            // Let the panel scheduler shorten the poll timeout when retrying.
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
                panel_scheduler::poll_timeout_ms(state.panel_schedule_state, flushable, 100)
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

            if fds[0].revents & libc::POLLIN != 0 {
                let frontend = self.frontend.as_mut().unwrap();
                if let Some(guard) = frontend.prepare_read() {
                    if let Err(e) = guard.read() {
                        eprintln!("Wayland read error: {e}");
                        return 1;
                    }
                }
            }
            if fds[0].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                eprintln!("Wayland display disconnected");
                return 1;
            }

            if fds[2].revents & libc::POLLIN != 0 {
                // Consume the timer expiration.
                let mut buf = [0u8; 8];
                unsafe {
                    libc::read(repeat_fd, buf.as_mut_ptr() as *mut c_void, buf.len());
                }
                let frontend = self.frontend.as_mut().unwrap();
                let state = frontend.state_mut();
                let router = self.router.as_mut().unwrap();
                if let Some(key) = router.repeat_key() {
                    state.forward_key(key.time, key.keycode, key.state);
                }
            }

        }

        eprintln!("typio: shutting down...");
        0
    }

    fn run_without_uds(&mut self) -> i32 {
        eprintln!("typio: running without UDS");

        #[cfg(feature = "wayland")]
        if let Some(ref mut frontend) = self.frontend {
            let _ = frontend.run();
            return 0;
        }

        while !shutdown_requested() {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        0
    }

    /// Tear down runtime services (currently just the instance).
    pub fn shutdown(&mut self) {
        if let Some(mut instance) = self.instance.take() {
            instance.shutdown_rust();
        }
    }

    /// Finalize: exec on restart, then return the exit code.
    pub fn finish(self, exit_code: i32) -> i32 {
        if (self.restart_requested || restart_requested_flag()) && exit_code == 0 {
            eprintln!("typio: restarting...");
            let argv0 = self.argv.first().cloned().unwrap_or_else(|| CString::new("typio").unwrap());
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
fn install_tray_action_handler(tray: &Tray, instance: *mut TypioInstance) {
    // Cast to usize so the closure is Send; reconstruct inside each arm.
    let instance_ptr = instance as usize;
    tray.set_action_handler(move |action| {
        let instance = instance_ptr as *mut TypioInstance;
        match action {
            TrayAction::Menu(MenuAction::Restart) => request_restart(),
            TrayAction::Menu(MenuAction::Quit) => request_shutdown(),
            TrayAction::Menu(MenuAction::Language(idx)) => {
                if let Some(tag) = language_at_index(instance, idx as usize) {
                    let _ = set_active_language(instance, &tag);
                }
            }
            TrayAction::Menu(MenuAction::EngineInLanguage { lang_idx: _, engine_idx }) => {
                if let Some(name) = keyboard_at_index(instance, engine_idx as usize) {
                    let _ = set_active_keyboard(instance, &name);
                }
            }
            TrayAction::Menu(MenuAction::OrphanEngine(idx)) => {
                if let Some(name) = orphan_keyboard_at_index(instance, idx as usize) {
                    let _ = set_active_keyboard(instance, &name);
                }
            }
            TrayAction::Menu(MenuAction::Voice(idx)) => {
                if let Some(name) = voice_at_index(instance, idx as usize) {
                    let _ = set_active_voice(instance, &name);
                }
            }
            _ => {}
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
            languages: info.effective_languages().iter().map(|s| s.to_string()).collect(),
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
                .map(|info| info.effective_languages().iter().all(|l| !known.contains(l)))
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

fn set_active_language(instance: *mut TypioInstance, tag: &str) -> Result<(), ()> {
    let reg = registry_ptr(instance).ok_or(())?;
    let tag_c = CString::new(tag).map_err(|_| ())?;
    match c_registry::typio_registry_set_active_language(reg, tag_c.as_ptr()) {
        typio::TypioResult::TypioOk => Ok(()),
        _ => Err(()),
    }
}

fn set_active_keyboard(instance: *mut TypioInstance, name: &str) -> Result<(), ()> {
    let reg = registry_ptr(instance).ok_or(())?;
    let name_c = CString::new(name).map_err(|_| ())?;
    match c_registry::typio_registry_set_active_keyboard(reg, name_c.as_ptr()) {
        typio::TypioResult::TypioOk => Ok(()),
        _ => Err(()),
    }
}

fn set_active_voice(instance: *mut TypioInstance, name: &str) -> Result<(), ()> {
    let reg = registry_ptr(instance).ok_or(())?;
    let name_c = CString::new(name).map_err(|_| ())?;
    match c_registry::typio_registry_set_active_voice(reg, name_c.as_ptr()) {
        typio::TypioResult::TypioOk => Ok(()),
        _ => Err(()),
    }
}

fn registry_ptr(instance: *mut TypioInstance) -> Option<*mut typio::c_api::registry::TypioRegistry> {
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
    let c_display = CString::new(manifest.display_name.as_deref().unwrap_or(&manifest.name)).ok()?;
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
        icon: c_icon.as_ref().map(|s| s.as_ptr()).unwrap_or(std::ptr::null()),
        language: c_lang.as_ptr(),
        type_: if manifest.engine_type == "voice" {
            typio_abi::TypioEngineType::TypioEngineTypeVoice
        } else {
            typio_abi::TypioEngineType::TypioEngineTypeKeyboard
        },
        required_capabilities: std::ptr::null(),
        optional_capabilities: std::ptr::null(),
    };

    let result = c_registry::typio_registry_register_engine_process(registry, &info, argv_ptrs.as_ptr());

    if result == TypioResult::TypioOk {
        // Leak the CStrings — the engine backend holds them for its lifetime.
        let name = manifest.name.clone();
        let engine_type = manifest.engine_type.clone();
        std::mem::forget(c_name);
        std::mem::forget(c_display);
        std::mem::forget(c_desc);
        std::mem::forget(c_author);
        std::mem::forget(c_icon);
        std::mem::forget(c_lang);
        std::mem::forget(argv_strings);
        Some((name, engine_type))
    } else {
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

    #[test]
    fn signal_flags_can_be_set_and_read() {
        let _guard = SIGNAL_FLAG_LOCK.lock().unwrap();

        // Reset to a known state.
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        RESTART_REQUESTED.store(false, Ordering::SeqCst);

        assert!(!shutdown_requested());
        request_shutdown();
        assert!(shutdown_requested());
        assert!(!restart_requested_flag());

        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        RESTART_REQUESTED.store(false, Ordering::SeqCst);

        request_restart();
        assert!(shutdown_requested());
        assert!(restart_requested_flag());

        // Leave the flags clear for any later test.
        SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
        RESTART_REQUESTED.store(false, Ordering::SeqCst);
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
        assert_eq!(snapshot.keyboards[0].display_name, Some("Fixture".to_string()));
        assert_eq!(snapshot.voices.len(), 0);

        typio::instance::typio_instance_free(inst);
    }
}
