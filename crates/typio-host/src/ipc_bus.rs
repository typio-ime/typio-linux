//! UDS request bus — wires [`UdsServer`] to [`StatusService`] and broadcasts
//! state changes to subscribed clients.
//!
//! Port of `src/ipc/ipc_bus.c`. The C module couples UDS framing, request
//! dispatch, and libtypio state mutation in one file. This Rust port keeps the
//! already-ported [`UdsServer`] and [`StatusService`] separate and only adds
//! the thin gluing layer plus a libtypio-backed [`ServiceBackend`] impl.
//!
//! ## Responsibilities
//!
//! - Install a request handler on the [`UdsServer`] that parses JSON-RPC,
//!   dispatches through [`StatusService`], and forwards any subscription change
//!   back to the server.
//! - Provide [`IpcBus::emit`] so a [`StateController`](crate::state_controller)
//!   listener can push notifications to subscribed UDS clients.
//! - Implement [`ServiceBackend`] for a raw [`TypioInstance`] pointer so the
//!   generic dispatch service can drive the live framework state.

use std::ffi::{CStr, CString};
use std::os::fd::RawFd;
use std::ptr;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::ipc::framing::{Id, Request, Response, StandardError};
use crate::service::{
    ConfigEntry, ConfigField, ConfigGetOutcome, ConfigSource, CycleLanguageOutcome, EngineCommand,
    EngineKind, FieldType, InvokeOutcome, RuntimeState, ServiceBackend, SvcError,
};
use crate::state_controller::RegistryView;
use crate::uds_server::{ClientId, RequestOutcome, SubscriptionUpdate, UdsServer};

/// The live UDS + dispatch surface.
pub struct IpcBus {
    server: UdsServer,
    service: Box<crate::service::StatusService<TypioBackend>>,
}

/// Opaque wrapper that makes a raw `*mut StatusService` safely sharable with the
/// UDS handler closure. The pointer is only ever dereferenced on the daemon's
/// main thread while [`IpcBus::dispatch`] runs; the Send/Sync impls are a type-
/// system workaround for the closure's Send bound.
struct UnsafeService(*mut crate::service::StatusService<TypioBackend>);
unsafe impl Send for UnsafeService {}
unsafe impl Sync for UnsafeService {}

impl IpcBus {
    /// Wrap a bound UDS server and a configured service. The constructor installs
    /// the JSON-RPC request handler on the server.
    pub fn new(server: UdsServer, service: crate::service::StatusService<TypioBackend>) -> Self {
        let mut service = Box::new(service);
        let service_ptr = service.as_mut() as *mut crate::service::StatusService<TypioBackend>;
        let wrapper = Arc::new(UnsafeService(service_ptr));

        let pending_sub: Arc<Mutex<Option<SubscriptionUpdate>>> = Arc::new(Mutex::new(None));

        let mut server = server;
        server.set_handler({
            let pending_sub = pending_sub.clone();
            move |json: &str, client_id: ClientId| {
                let svc = wrapper.0;

                let req = match Request::parse(json) {
                    Ok(r) => r,
                    Err(_) => {
                        let resp = Response::error(
                            Id::Null,
                            StandardError::ParseError.code(),
                            "Parse error",
                        );
                        return RequestOutcome::respond(resp.to_json().unwrap_or_default());
                    }
                };

                let id = match &req.id {
                    Id::Number(n) => *n,
                    _ => 0,
                };
                let params = req.params.as_ref().cloned().unwrap_or(Value::Null);

                // Capture the subscription request (if any) so it can be applied
                // by the server after this closure returns.
                let pending = pending_sub.clone();
                unsafe { &mut (*svc) }.set_subscribe_callback({
                    let p = pending.clone();
                    move |_token, topics| {
                        let update = if topics.is_empty() {
                            SubscriptionUpdate::Wildcard
                        } else {
                            SubscriptionUpdate::Topics(topics)
                        };
                        *p.lock().unwrap() = Some(update);
                    }
                });

                let resp = unsafe { &mut (*svc) }.handle(&req.method, &params, id, client_id.0);
                let sub = pending.lock().unwrap().take();

                let json_resp = match resp.to_json() {
                    Ok(s) => s,
                    Err(_) => return RequestOutcome::silent(),
                };

                match sub {
                    Some(update) => RequestOutcome::respond_and_subscribe(json_resp, update),
                    None => RequestOutcome::respond(json_resp),
                }
            }
        });

        Self { server, service }
    }

    /// Install the callback triggered by `daemon.stop`.
    pub fn set_stop_callback<F: FnMut() + 'static>(&mut self, cb: F) {
        self.service.set_stop_callback(cb);
    }

    /// Drain pending UDS events. Call once per loop iteration.
    pub fn dispatch(&mut self) {
        self.server.dispatch();
    }

    /// Emit a JSON-RPC notification to every subscribed client matching `topic`.
    pub fn emit(&mut self, topic: &str, payload: &Value) {
        self.server.emit(topic, payload);
    }

    /// The socket path the underlying server is bound to.
    pub fn socket_path(&self) -> std::path::PathBuf {
        self.server.socket_path().to_path_buf()
    }

    /// The epoll fd of the underlying server, suitable for polling.
    pub fn epoll_fd(&self) -> RawFd {
        self.server.epoll_fd()
    }
}

// ── libtypio-backed ServiceBackend ───────────────────────────────────────────

/// A [`ServiceBackend`] that drives a live [`TypioInstance`] through its public
/// C ABI (and the small Rust-native registry accessor where available).
pub struct TypioBackend {
    instance: *mut typio::TypioInstance,
}

impl TypioBackend {
    /// The instance pointer must remain valid for the lifetime of the backend.
    pub fn new(instance: *mut typio::TypioInstance) -> Self {
        Self { instance }
    }

    fn instance(&self) -> Option<&typio::TypioInstance> {
        unsafe { self.instance.as_ref() }
    }

    fn registry_ptr(&self) -> *mut typio::c_api::registry::TypioRegistry {
        if self.instance.is_null() {
            return ptr::null_mut();
        }
        typio::instance::typio_instance_get_registry(self.instance)
    }

    fn config_ptr(&self) -> *mut typio::config::Config {
        if self.instance.is_null() {
            return ptr::null_mut();
        }
        typio::instance::typio_instance_get_config(self.instance)
    }

    fn registry(&self) -> Option<&typio::core::registry::EngineRegistry> {
        self.instance().and_then(|i| i.registry_rust())
    }
}

impl ServiceBackend for TypioBackend {
    // ── config ──

    fn config_get(&self, key: &str) -> Option<ConfigGetOutcome> {
        let cfg = self.config_ptr();
        if cfg.is_null() {
            return None;
        }
        if !typio::config::typio_config_has_key(cfg, c_str(key).as_ptr()) {
            return Some(ConfigGetOutcome::Unknown);
        }
        let value = typio::config::typio_config_get_string(cfg, c_str(key).as_ptr(), ptr::null());
        let s = if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned()
        };
        Some(ConfigGetOutcome::Found {
            value: Value::String(s),
            field_type: FieldType::String,
            source: ConfigSource::Default,
        })
    }

    fn config_set(&mut self, key: &str, value_str: &str) -> Option<Result<(), SvcError>> {
        let cfg = self.config_ptr();
        if cfg.is_null() {
            return None;
        }
        set_config_value(cfg, key, value_str);
        Some(Ok(()))
    }

    fn config_unset(&mut self, key: &str) -> Option<Result<(), SvcError>> {
        let cfg = self.config_ptr();
        if cfg.is_null() {
            return None;
        }
        let key_c = c_str(key);
        match typio::config::typio_config_remove(cfg, key_c.as_ptr()) {
            typio::TypioResult::TypioOk => Some(Ok(())),
            typio::TypioResult::TypioErrorNotFound => Some(Err(SvcError)),
            _ => Some(Err(SvcError)),
        }
    }

    fn config_list(&self, prefix: &str) -> Option<Vec<ConfigEntry>> {
        let cfg = self.config_ptr();
        if cfg.is_null() {
            return None;
        }
        let count = typio::config::typio_config_key_count(cfg);
        let mut entries = Vec::new();
        for i in 0..count {
            let key_ptr = typio::config::typio_config_key_at(cfg, i);
            if key_ptr.is_null() {
                continue;
            }
            let key = unsafe { CStr::from_ptr(key_ptr) }
                .to_string_lossy()
                .into_owned();
            typio::string::typio_free_string(key_ptr);
            if !prefix.is_empty() && !key.starts_with(prefix) {
                continue;
            }
            let value =
                typio::config::typio_config_get_string(cfg, c_str(&key).as_ptr(), ptr::null());
            let value_str = if value.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(value) }
                    .to_string_lossy()
                    .into_owned()
            };
            entries.push(ConfigEntry {
                field: ConfigField {
                    key,
                    field_type: FieldType::String,
                    label: None,
                    section: None,
                    choices: None,
                },
                value: Value::String(value_str),
                source: ConfigSource::Default,
            });
        }
        Some(entries)
    }

    fn config_show_text(&self) -> String {
        if self.instance.is_null() {
            return String::new();
        }
        let text = typio::instance::typio_instance_get_config_text(self.instance);
        if text.is_null() {
            return String::new();
        }
        let s = unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned();
        typio::string::typio_free_string(text);
        s
    }

    fn config_reload(&mut self) -> Result<(), SvcError> {
        if self.instance.is_null() {
            return Err(SvcError);
        }
        match typio::instance::typio_instance_reload_config(self.instance) {
            typio::TypioResult::TypioOk => Ok(()),
            _ => Err(SvcError),
        }
    }

    fn save_config(&mut self) {
        if !self.instance.is_null() {
            typio::instance::typio_instance_save_config(self.instance);
        }
    }

    fn notify_engine_config(&mut self, engine: &str, key: &str, value: &str) {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return;
        }
        let engine_c = c_str(engine);
        let key_c = c_str(key);
        let value_c = c_str(value);
        typio::c_api::registry::typio_registry_notify_config_change(
            reg,
            engine_c.as_ptr(),
            key_c.as_ptr(),
            value_c.as_ptr(),
        );
    }

    // ── registry ──

    fn registry_present(&self) -> bool {
        self.registry().is_some()
    }

    fn list_keyboards(&self) -> Vec<String> {
        self.registry()
            .map(|r| r.list_keyboards().into_iter().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn list_voices(&self) -> Vec<String> {
        self.registry()
            .map(|r| r.list_voices().into_iter().map(str::to_string).collect())
            .unwrap_or_default()
    }

    fn list_languages(&self) -> Vec<String> {
        self.registry()
            .map(|r| r.known_languages())
            .unwrap_or_default()
    }

    fn engine_info(&self, name: &str) -> Option<EngineKind> {
        self.registry()
            .and_then(|r| r.engine_info(name))
            .map(|info| match info.engine_type {
                typio::core::engine::EngineType::Keyboard => EngineKind::Keyboard,
                typio::core::engine::EngineType::Voice => EngineKind::Voice,
            })
    }

    fn engine_display_name(&self, name: &str) -> Option<String> {
        self.registry()
            .and_then(|r| r.engine_info(name))
            .map(|info| info.display_name.clone())
    }

    fn active_keyboard(&self) -> Option<String> {
        self.registry()
            .and_then(|r| r.active_keyboard_name())
            .map(str::to_string)
    }

    fn active_voice(&self) -> Option<String> {
        self.registry()
            .and_then(|r| r.active_voice_name())
            .map(str::to_string)
    }

    fn active_language(&self) -> Option<String> {
        self.registry()
            .and_then(|r| r.active_language())
            .map(str::to_string)
    }

    fn set_active_keyboard(&mut self, name: &str) -> Result<(), SvcError> {
        self.set_active_engine(name, false)
    }

    fn set_active_voice(&mut self, name: &str) -> Result<(), SvcError> {
        self.set_active_engine(name, true)
    }

    fn set_active_language(&mut self, tag: &str) -> Result<(), SvcError> {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return Err(SvcError);
        }
        let tag_c = c_str(tag);
        match typio::c_api::registry::typio_registry_set_active_language(reg, tag_c.as_ptr()) {
            typio::TypioResult::TypioOk => Ok(()),
            _ => Err(SvcError),
        }
    }

    fn cycle_keyboard(&mut self, forward: bool) -> Result<(), SvcError> {
        self.cycle_engine(forward, false)
    }

    fn cycle_voice(&mut self, forward: bool) -> Result<(), SvcError> {
        self.cycle_engine(forward, true)
    }

    fn cycle_language(&mut self, forward: bool) -> CycleLanguageOutcome {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return CycleLanguageOutcome::Failed;
        }
        let result = if forward {
            typio::c_api::registry::typio_registry_next_language(reg)
        } else {
            typio::c_api::registry::typio_registry_prev_language(reg)
        };
        match result {
            typio::TypioResult::TypioOk => CycleLanguageOutcome::Ok(self.active_language()),
            typio::TypioResult::TypioErrorNotFound => CycleLanguageOutcome::NoLanguages,
            _ => CycleLanguageOutcome::Failed,
        }
    }

    fn list_commands(&self, name: &str) -> Vec<EngineCommand> {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return Vec::new();
        }
        let name_c = c_str(name);
        let mut count: usize = 0;
        let commands =
            typio::c_api::registry::typio_registry_list_commands(reg, name_c.as_ptr(), &mut count);
        if commands.is_null() || count == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let cmd = unsafe { &*commands.add(i) };
            let id = if cmd.id.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(cmd.id) }
                    .to_string_lossy()
                    .into_owned()
            };
            let label = if cmd.label.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(cmd.label) }
                    .to_string_lossy()
                    .into_owned()
            };
            out.push(EngineCommand { id, label });
        }
        typio::c_api::registry::typio_engine_command_list_free(commands, count);
        out
    }

    fn invoke_command(&mut self, name: &str, cmd: &str) -> InvokeOutcome {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return InvokeOutcome::Failed;
        }
        let name_c = c_str(name);
        let cmd_c = c_str(cmd);
        match typio::c_api::registry::typio_registry_invoke_command(
            reg,
            name_c.as_ptr(),
            cmd_c.as_ptr(),
        ) {
            typio::TypioResult::TypioOk => InvokeOutcome::Ok,
            typio::TypioResult::TypioErrorNotFound => InvokeOutcome::NotFound,
            typio::TypioResult::TypioErrorEngineNotAvailable => InvokeOutcome::NotSupported,
            _ => InvokeOutcome::Failed,
        }
    }

    // ── engine loader ──
    //
    // engine.load/reload are not yet wired from the host: the registry inner
    // is crate-private, so we cannot drive EngineLoader directly. The C ABI
    // exposes unload; load/reload return "not supported" until a host-side
    // registration helper is added.

    fn engine_load(&mut self, _path: &str) -> Result<(), SvcError> {
        Err(SvcError)
    }

    fn engine_unload(&mut self, name: &str) -> Result<(), SvcError> {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return Err(SvcError);
        }
        let name_c = c_str(name);
        match typio::c_api::registry::typio_registry_unload(reg, name_c.as_ptr()) {
            typio::TypioResult::TypioOk => Ok(()),
            typio::TypioResult::TypioErrorNotFound => Err(SvcError),
            _ => Err(SvcError),
        }
    }

    fn engine_reload(&mut self, _name: &str, _path: Option<&str>) -> Result<(), SvcError> {
        Err(SvcError)
    }

    // ── daemon ──

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn runtime_state(&self) -> Option<RuntimeState> {
        None
    }
}

impl TypioBackend {
    fn set_active_engine(&self, name: &str, voice: bool) -> Result<(), SvcError> {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return Err(SvcError);
        }
        let name_c = c_str(name);
        let result = if voice {
            typio::c_api::registry::typio_registry_set_active_voice(reg, name_c.as_ptr())
        } else {
            typio::c_api::registry::typio_registry_set_active_keyboard(reg, name_c.as_ptr())
        };
        match result {
            typio::TypioResult::TypioOk => Ok(()),
            _ => Err(SvcError),
        }
    }

    fn cycle_engine(&self, forward: bool, voice: bool) -> Result<(), SvcError> {
        let reg = self.registry_ptr();
        if reg.is_null() {
            return Err(SvcError);
        }
        let result = match (forward, voice) {
            (true, false) => typio::c_api::registry::typio_registry_next_keyboard(reg),
            (false, false) => typio::c_api::registry::typio_registry_prev_keyboard(reg),
            (true, true) => typio::c_api::registry::typio_registry_next_voice(reg),
            (false, true) => typio::c_api::registry::typio_registry_prev_voice(reg),
        };
        match result {
            typio::TypioResult::TypioOk => Ok(()),
            _ => Err(SvcError),
        }
    }
}

// ── RegistryView adapter for StateController ─────────────────────────────────

/// A [`RegistryView`] backed by the live [`TypioInstance`].
pub struct TypioRegistryView {
    instance: *mut typio::TypioInstance,
}

impl TypioRegistryView {
    /// The instance pointer must remain valid for the lifetime of the view.
    pub fn new(instance: *mut typio::TypioInstance) -> Self {
        Self { instance }
    }

    fn registry(&self) -> Option<&typio::core::registry::EngineRegistry> {
        unsafe { self.instance.as_ref() }.and_then(|i| i.registry_rust())
    }

    fn config_string(&self, key: &str) -> Option<String> {
        if self.instance.is_null() {
            return None;
        }
        let cfg = typio::instance::typio_instance_get_config(self.instance);
        if cfg.is_null() {
            return None;
        }
        let key_c = c_str(key);
        let value = typio::config::typio_config_get_string(cfg, key_c.as_ptr(), ptr::null());
        if value.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(value) }
            .to_string_lossy()
            .into_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

impl RegistryView for TypioRegistryView {
    fn active_keyboard(&self) -> Option<String> {
        self.registry()
            .and_then(|r| r.active_keyboard_name())
            .map(str::to_string)
    }

    fn active_language(&self) -> Option<String> {
        self.registry()
            .and_then(|r| r.active_language())
            .map(str::to_string)
    }

    fn active_voice(&self) -> Option<String> {
        self.registry()
            .and_then(|r| r.active_voice_name())
            .map(str::to_string)
    }

    fn engine_display_name(&self, name: &str) -> Option<String> {
        self.registry()
            .and_then(|r| r.engine_info(name))
            .map(|info| info.display_name.clone())
    }

    fn config_icon(&self, key: &str) -> Option<String> {
        self.config_string(key)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn c_str(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| CString::default())
}

fn set_config_value(config: *mut typio::config::Config, key: &str, raw: &str) {
    let key_c = c_str(key);
    if raw == "true" {
        typio::config::typio_config_set_bool(config, key_c.as_ptr(), true);
        return;
    }
    if raw == "false" {
        typio::config::typio_config_set_bool(config, key_c.as_ptr(), false);
        return;
    }
    if let Ok(i) = raw.parse::<i32>() {
        typio::config::typio_config_set_int(config, key_c.as_ptr(), i);
        return;
    }
    if let Ok(f) = raw.parse::<f64>() {
        typio::config::typio_config_set_float(config, key_c.as_ptr(), f);
        return;
    }
    let value_c = c_str(raw);
    typio::config::typio_config_set_string(config, key_c.as_ptr(), value_c.as_ptr());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_str_handles_nul() {
        // A key/value containing an interior NUL falls back to the empty string
        // rather than panicking; the C ABI would reject it anyway.
        assert_eq!(c_str("foo\0bar").to_bytes().len(), 0);
    }

    #[test]
    fn config_value_heuristic() {
        // These match the service FieldType coercion semantics for the common
        // textual forms.
        assert!(parse_config_heuristic("true").is_boolean());
        assert!(parse_config_heuristic("42").is_i64());
        assert!(parse_config_heuristic("3.14").is_f64());
        assert!(parse_config_heuristic("hello").is_string());
    }

    fn parse_config_heuristic(raw: &str) -> Value {
        if raw == "true" {
            return Value::Bool(true);
        }
        if raw == "false" {
            return Value::Bool(false);
        }
        if let Ok(i) = raw.parse::<i64>() {
            return Value::Number(i.into());
        }
        if let Ok(f) = raw.parse::<f64>() {
            return serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null);
        }
        Value::String(raw.to_string())
    }
}
