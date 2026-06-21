//! Transport-agnostic TIP dispatch — the daemon's method router.
//!
//! Port of `src/state/service.c`. This module owns the *dispatch policy* of the
//! Typio IPC Protocol: it parses a method's params, queries the engine/config
//! state via the [`ServiceBackend`] trait, and builds the JSON-RPC 2.0 response.
//! It knows nothing about UDS framing — that lives in [`crate::uds_server`].
//!
//! ## Design
//!
//! The C original reaches directly into a live `TypioInstance` / `TypioRegistry`
//! / `TypioConfig`. To keep this port unit-testable without a full libtypio
//! fixture, every libtypio read/write is abstracted behind the [`ServiceBackend`]
//! trait. The real (libtypio-backed) impl is a separate concern; here the
//! dispatch is generic over `B: ServiceBackend`.
//!
//! Error messages are reproduced verbatim from the C handler so clients and the
//! existing C-test parity assertions continue to hold.

use std::time::Instant;

use serde_json::{json, Value};

use crate::ipc::framing::{Id, Response, StandardError};
use crate::ipc::protocol;

/// Opaque transport-side identifier for the calling client. Forwarded to the
/// subscribe callback. Modelled as a `u64` connection id (the C version uses a
/// `void *` pointer); the UDS transport supplies the real value.
pub type ClientToken = u64;

// ── Data model ───────────────────────────────────────────────────────────

/// A config field's value type. Mirrors `TypioFieldType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    String,
    Int,
    Bool,
    Float,
}

impl FieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Int => "int",
            FieldType::Bool => "bool",
            FieldType::Float => "float",
        }
    }

    /// Parse a C-style textual value into the typed JSON value, exactly as the
    /// C `handle_config_set` does (`strtol` / `strtod` / `"true"|""1`).
    pub fn coerce_from_str(self, raw: &str) -> Value {
        match self {
            FieldType::String => Value::String(raw.to_string()),
            FieldType::Int => Value::Number(raw.trim().parse::<i64>().unwrap_or(0).into()),
            FieldType::Bool => Value::Bool(raw == "true" || raw == "1"),
            FieldType::Float => {
                serde_json::Number::from_f64(raw.trim().parse::<f64>().unwrap_or(0.0))
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            }
        }
    }
}

/// Where a config value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// Explicitly set by the user.
    User,
    /// Taken from the schema default.
    Default,
}

impl ConfigSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigSource::User => "user",
            ConfigSource::Default => "default",
        }
    }
}

/// A schema-described config field.
#[derive(Debug, Clone)]
pub struct ConfigField {
    pub key: String,
    pub field_type: FieldType,
    pub label: Option<String>,
    pub section: Option<String>,
    pub choices: Option<Vec<String>>,
}

/// A resolved config entry: the field descriptor plus its current value and
/// source.
#[derive(Debug, Clone)]
pub struct ConfigEntry {
    pub field: ConfigField,
    pub value: Value,
    pub source: ConfigSource,
}

/// Keyboard vs. voice engine. Mirrors `TypioEngineType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Keyboard,
    Voice,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Keyboard => "keyboard",
            EngineKind::Voice => "voice",
        }
    }
}

/// An engine command exposed via `engine.describe`.
#[derive(Debug, Clone)]
pub struct EngineCommand {
    pub id: String,
    pub label: String,
}

/// Snapshot of the daemon's runtime state, surfaced through `daemon.status`
/// when a runtime-state provider is wired. Mirrors `TypioStatusRuntimeState`.
#[derive(Debug, Clone, Default)]
pub struct RuntimeState {
    pub frontend_backend: String,
    pub lifecycle_phase: String,
    pub virtual_keyboard_state: String,
    pub keyboard_grab_active: bool,
    pub virtual_keyboard_has_keymap: bool,
    pub watchdog_armed: bool,
    pub active_key_generation: u32,
    pub virtual_keyboard_keymap_generation: u32,
    pub virtual_keyboard_drop_count: u32,
    pub virtual_keyboard_state_age_ms: u32,
    pub virtual_keyboard_keymap_age_ms: u32,
    pub virtual_keyboard_forward_age_ms: u32,
    pub virtual_keyboard_keymap_deadline_remaining_ms: i32,
}

// ── Backend trait ────────────────────────────────────────────────────────

/// Outcome of a `config.get` lookup.
pub enum ConfigGetOutcome {
    Found {
        value: Value,
        field_type: FieldType,
        source: ConfigSource,
    },
    /// Key matches no schema field and no user value.
    Unknown,
}

/// Outcome of `language.next`/`language.prev`. Mirrors the three branches of
/// `handle_language_cycle`.
#[derive(Debug, Clone)]
pub enum CycleLanguageOutcome {
    Ok(Option<String>),
    /// `TYPIO_ERROR_NOT_FOUND` — no languages enabled/declared.
    NoLanguages,
    /// Any other failure.
    Failed,
}

/// Outcome of `engine.invoke`. Mirrors the `TypioResult` switch.
/// A generic error type for service backend operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SvcError;

impl std::fmt::Display for SvcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "service error")
    }
}

impl std::error::Error for SvcError {}

#[derive(Debug, Clone, Default)]
pub enum InvokeOutcome {

    #[default]
    Ok,
    /// `TYPIO_ERROR_NOT_FOUND`.
    NotFound,
    /// `TYPIO_ERROR_ENGINE_NOT_AVAILABLE` — the command exists but the engine
    /// doesn't implement it.
    NotSupported,
    Failed,
}

impl Default for CycleLanguageOutcome {
    fn default() -> Self {
        CycleLanguageOutcome::Ok(None)
    }
}

/// The libtypio surface the dispatch needs. Implementations may back this with
/// a live `TypioInstance` (production) or a fake (tests).
pub trait ServiceBackend {
    // ── config ──
    /// `None` if the config object is unavailable (mirrors `svc_config` → null).
    fn config_get(&self, key: &str) -> Option<ConfigGetOutcome>;
    /// `None` if config unavailable; `Err(SvcError)` if the underlying set failed.
    fn config_set(&mut self, key: &str, value_str: &str) -> Option<Result<(), SvcError>>;
    /// `None` if config unavailable; `Err(SvcError)` if the key was unknown.
    fn config_unset(&mut self, key: &str) -> Option<Result<(), SvcError>>;
    /// `None` if config unavailable.
    fn config_list(&self, prefix: &str) -> Option<Vec<ConfigEntry>>;
    /// The full TOML text. Empty string if unavailable (the C version never
    /// errors on `config.show`, it returns `""`).
    fn config_show_text(&self) -> String;
    /// `Err(SvcError)` if reload failed.
    fn config_reload(&mut self) -> Result<(), SvcError>;
    /// Persist the current config to disk (`typio_instance_save_config`).
    fn save_config(&mut self);
    /// Notify an engine that one of its config keys changed
    /// (`typio_registry_notify_config_change`). Called only when the changed
    /// key sits under the engine's namespace.
    fn notify_engine_config(&mut self, engine: &str, key: &str, value: &str);

    // ── registry ──
    /// Whether a registry is attached. When `false`, registry-using handlers
    /// return `"no registry"`.
    fn registry_present(&self) -> bool;
    fn list_keyboards(&self) -> Vec<String>;
    fn list_voices(&self) -> Vec<String>;
    fn list_languages(&self) -> Vec<String>;
    fn engine_info(&self, name: &str) -> Option<EngineKind>;
    /// Display name for an engine, if known.
    fn engine_display_name(&self, name: &str) -> Option<String>;
    fn active_keyboard(&self) -> Option<String>;
    fn active_voice(&self) -> Option<String>;
    fn active_language(&self) -> Option<String>;
    fn set_active_keyboard(&mut self, name: &str) -> Result<(), SvcError>;
    fn set_active_voice(&mut self, name: &str) -> Result<(), SvcError>;
    fn set_active_language(&mut self, tag: &str) -> Result<(), SvcError>;
    fn cycle_keyboard(&mut self, forward: bool) -> Result<(), SvcError>;
    fn cycle_voice(&mut self, forward: bool) -> Result<(), SvcError>;
    fn cycle_language(&mut self, forward: bool) -> CycleLanguageOutcome;
    fn list_commands(&self, name: &str) -> Vec<EngineCommand>;
    fn invoke_command(&mut self, name: &str, cmd: &str) -> InvokeOutcome;

    // ── engine loader ──
    fn engine_load(&mut self, path: &str) -> Result<(), SvcError>;
    fn engine_unload(&mut self, name: &str) -> Result<(), SvcError>;
    fn engine_reload(&mut self, name: &str, path: Option<&str>) -> Result<(), SvcError>;

    // ── daemon ──
    fn version(&self) -> &str;
    fn runtime_state(&self) -> Option<RuntimeState>;
}

// ── Callbacks ────────────────────────────────────────────────────────────

type StopCallback = Box<dyn FnMut()>;
type SubscribeCallback = Box<dyn FnMut(ClientToken, Vec<String>)>;

// ── Service ──────────────────────────────────────────────────────────────

/// TIP dispatch service. Generic over the backend; holds optional callbacks
/// for the few methods (`daemon.stop`, `events.subscribe`) that have side
/// effects the daemon core must own.
pub struct StatusService<B: ServiceBackend> {
    backend: B,
    stop_callback: Option<StopCallback>,
    subscribe_callback: Option<SubscribeCallback>,
    started_at: Instant,
}

impl<B: ServiceBackend> StatusService<B> {
    pub fn new(backend: B) -> Self {
        StatusService {
            backend,
            stop_callback: None,
            subscribe_callback: None,
            started_at: Instant::now(),
        }
    }

    pub fn set_stop_callback<F: FnMut() + 'static>(&mut self, cb: F) {
        self.stop_callback = Some(Box::new(cb));
    }

    pub fn set_subscribe_callback<F: FnMut(ClientToken, Vec<String>) + 'static>(&mut self, cb: F) {
        self.subscribe_callback = Some(Box::new(cb));
    }

    /// Reset the uptime origin (the C version records `started_at` at
    /// construction; tests use this to make uptime deterministic).
    pub fn reset_uptime(&mut self) {
        self.started_at = Instant::now();
    }

    /// Top-level dispatch. Mirrors `typio_status_service_handle`.
    pub fn handle(
        &mut self,
        method: &str,
        params: &Value,
        id: i64,
        client_token: ClientToken,
    ) -> Response {
        use protocol::methods as m;

        match method {
            m::HELLO => self.handle_hello(id),
            m::CONFIG_GET => self.handle_config_get(params, id),
            m::CONFIG_SET => self.handle_config_set(params, id),
            m::CONFIG_UNSET => self.handle_config_unset(params, id),
            m::CONFIG_LIST => self.handle_config_list(params, id),
            m::CONFIG_SHOW => self.handle_config_show(id),
            m::CONFIG_RELOAD => self.handle_config_reload(id),
            m::ENGINE_LIST => self.handle_engine_list(id),
            m::ENGINE_DESCRIBE => self.handle_engine_describe(params, id),
            m::KEYBOARD_USE => self.handle_modal_use(params, id, false),
            m::VOICE_USE => self.handle_modal_use(params, id, true),
            m::KEYBOARD_NEXT => self.handle_modal_cycle(id, false, true),
            m::KEYBOARD_PREV => self.handle_modal_cycle(id, false, false),
            m::VOICE_NEXT => self.handle_modal_cycle(id, true, true),
            m::VOICE_PREV => self.handle_modal_cycle(id, true, false),
            m::LANGUAGE_LIST => self.handle_language_list(id),
            m::LANGUAGE_USE => self.handle_language_use(params, id),
            m::LANGUAGE_NEXT => self.handle_language_cycle(id, true),
            m::LANGUAGE_PREV => self.handle_language_cycle(id, false),
            m::ENGINE_INVOKE => self.handle_engine_invoke(params, id),
            m::ENGINE_LOAD => self.handle_engine_load(params, id),
            m::ENGINE_UNLOAD => self.handle_engine_unload(params, id),
            m::ENGINE_RELOAD => self.handle_engine_reload(params, id),
            m::DAEMON_STATUS => self.handle_daemon_status(id),
            m::DAEMON_STOP => self.handle_daemon_stop(id),
            m::DAEMON_VERSION => self.handle_daemon_version(id),
            m::EVENTS_SUBSCRIBE => self.handle_events_subscribe(params, id, client_token),
            _ => err(id, StandardError::MethodNotFound, "Method not found"),
        }
    }

    // ── hello ──

    fn handle_hello(&self, id: i64) -> Response {
        let result = json!({
            "protocolVersion": protocol::PROTOCOL_VERSION,
            "daemonVersion": self.backend.version(),
            "capabilities": ["config", "engine", "keyboard", "voice",
                             "language", "daemon", "events"],
        });
        ok(id, result)
    }

    // ── config.* ──

    fn handle_config_get(&self, params: &Value, id: i64) -> Response {
        let Some(key) = get_str(params, "key") else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'key' param");
        };
        match self.backend.config_get(key) {
            None => err_msg(id, StandardError::InternalError, "Config unavailable"),
            Some(ConfigGetOutcome::Unknown) => {
                err_msg(id, StandardError::InvalidParams, "Unknown key")
            }
            Some(ConfigGetOutcome::Found {
                value,
                field_type,
                source,
            }) => ok(
                id,
                json!({
                    "value": value,
                    "type": field_type.as_str(),
                    "source": source.as_str(),
                }),
            ),
        }
    }

    fn handle_config_set(&mut self, params: &Value, id: i64) -> Response {
        let Some(key) = get_str(params, "key") else {
            return err_msg(
                id,
                StandardError::InvalidParams,
                "Missing 'key' or 'value' param",
            );
        };
        let Some(value_str) = get_str(params, "value") else {
            return err_msg(
                id,
                StandardError::InvalidParams,
                "Missing 'key' or 'value' param",
            );
        };
        match self.backend.config_set(key, value_str) {
            None => err_msg(id, StandardError::InternalError, "Config unavailable"),
            Some(Err(SvcError)) => err_msg(id, StandardError::InternalError, "config.set failed"),
            Some(Ok(())) => {
                self.backend.save_config();
                if let Some((engine, _sub)) = parse_engine_namespace(key) {
                    self.backend.notify_engine_config(&engine, key, value_str);
                }
                ok(id, json!({}))
            }
        }
    }

    fn handle_config_unset(&mut self, params: &Value, id: i64) -> Response {
        let Some(key) = get_str(params, "key") else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'key' param");
        };
        match self.backend.config_unset(key) {
            None => err_msg(id, StandardError::InternalError, "Config unavailable"),
            Some(Err(SvcError)) => err_msg(id, StandardError::InvalidParams, "Unknown key"),
            Some(Ok(())) => {
                self.backend.save_config();
                ok(id, json!({}))
            }
        }
    }

    fn handle_config_list(&self, params: &Value, id: i64) -> Response {
        let prefix = get_str(params, "prefix").unwrap_or_default();
        match self.backend.config_list(prefix) {
            None => err_msg(id, StandardError::InternalError, "Config unavailable"),
            Some(entries) => {
                let arr: Vec<Value> = entries.into_iter().map(entry_to_json).collect();
                ok(id, Value::Array(arr))
            }
        }
    }

    fn handle_config_show(&self, id: i64) -> Response {
        let text = self.backend.config_show_text();
        ok(id, json!({ "text": text, "format": "toml" }))
    }

    fn handle_config_reload(&mut self, id: i64) -> Response {
        match self.backend.config_reload() {
            Ok(()) => ok(id, json!({})),
            Err(SvcError) => err_msg(id, StandardError::InternalError, "config.reload failed"),
        }
    }

    // ── engine.* ──

    fn handle_engine_list(&self, id: i64) -> Response {
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        let active_kb = self.backend.active_keyboard();
        let active_voice = self.backend.active_voice();
        let mut items = Vec::new();
        for kb in self.backend.list_keyboards() {
            items.push(engine_summary(
                &kb,
                EngineKind::Keyboard,
                &active_kb,
                self.backend.engine_display_name(&kb).as_deref(),
            ));
        }
        for v in self.backend.list_voices() {
            items.push(engine_summary(
                &v,
                EngineKind::Voice,
                &active_voice,
                self.backend.engine_display_name(&v).as_deref(),
            ));
        }
        ok(id, Value::Array(items))
    }

    fn handle_engine_describe(&self, params: &Value, id: i64) -> Response {
        let Some(name) = get_str(params, "name") else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'name' param");
        };
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        let Some(kind) = self.backend.engine_info(name) else {
            return err_msg(id, StandardError::InvalidParams, "Unknown engine");
        };
        let display = self
            .backend
            .engine_display_name(name)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| name.to_string());

        let prefix = format!("engines.{name}.");
        let props = self.backend.config_list(&prefix).unwrap_or_default();
        let properties: Vec<Value> = props.into_iter().map(entry_to_json).collect();
        let commands: Vec<Value> = self
            .backend
            .list_commands(name)
            .into_iter()
            .map(|c| json!({ "id": c.id, "label": c.label }))
            .collect();

        ok(
            id,
            json!({
                "name": name,
                "kind": kind.as_str(),
                "displayName": display,
                "properties": properties,
                "commands": commands,
            }),
        )
    }

    /// `keyboard.use` (voice=false) / `voice.use` (voice=true). ADR-0026:
    /// the verb fixes the modality; the engine must match it.
    fn handle_modal_use(&mut self, params: &Value, id: i64, voice: bool) -> Response {
        let name = get_str(params, "name").filter(|s| !s.is_empty());
        let Some(name) = name else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'name' param");
        };
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        let Some(kind) = self.backend.engine_info(name) else {
            return err_msg(id, StandardError::InvalidParams, "Unknown engine");
        };
        let is_voice = kind == EngineKind::Voice;
        if is_voice != voice {
            return err_msg(
                id,
                StandardError::InvalidParams,
                if voice {
                    "Not a voice engine"
                } else {
                    "Not a keyboard engine"
                },
            );
        }
        let r = if voice {
            self.backend.set_active_voice(name)
        } else {
            self.backend.set_active_keyboard(name)
        };
        if r.is_err() {
            return err_msg(
                id,
                StandardError::InternalError,
                if voice {
                    "voice.use failed"
                } else {
                    "keyboard.use failed"
                },
            );
        }
        self.backend.save_config();
        ok(id, json!({}))
    }

    /// `keyboard.next/prev` / `voice.next/prev`.
    fn handle_modal_cycle(&mut self, id: i64, voice: bool, forward: bool) -> Response {
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        let r = if voice {
            self.backend.cycle_voice(forward)
        } else {
            self.backend.cycle_keyboard(forward)
        };
        if r.is_err() {
            return err_msg(id, StandardError::InternalError, "cycle failed");
        }
        let active = if voice {
            self.backend.active_voice()
        } else {
            self.backend.active_keyboard()
        };
        ok(id, json!({ "active": active.unwrap_or_default() }))
    }

    // ── language.* (ADR-0031) ──

    fn handle_language_list(&self, id: i64) -> Response {
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        let active = self.backend.active_language();
        let tags = self.backend.list_languages();
        let langs: Vec<Value> = tags
            .iter()
            .map(|t| {
                let is_active = active.as_deref() == Some(t.as_str());
                json!({ "tag": t, "active": is_active })
            })
            .collect();
        ok(
            id,
            json!({
                "languages": langs,
                "active": active.unwrap_or_default(),
            }),
        )
    }

    fn handle_language_use(&mut self, params: &Value, id: i64) -> Response {
        let tag = get_str(params, "tag").filter(|s| !s.is_empty());
        let Some(tag) = tag else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'tag' param");
        };
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        if self.backend.set_active_language(tag).is_err() {
            return err_msg(id, StandardError::InternalError, "language.use failed");
        }
        self.backend.save_config();
        ok(id, json!({}))
    }

    fn handle_language_cycle(&mut self, id: i64, forward: bool) -> Response {
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        match self.backend.cycle_language(forward) {
            CycleLanguageOutcome::Ok(active) => {
                ok(id, json!({ "active": active.unwrap_or_default() }))
            }
            CycleLanguageOutcome::NoLanguages => err_msg(
                id,
                StandardError::InvalidParams,
                "No languages enabled or declared",
            ),
            CycleLanguageOutcome::Failed => {
                err_msg(id, StandardError::InternalError, "language cycle failed")
            }
        }
    }

    fn handle_engine_invoke(&mut self, params: &Value, id: i64) -> Response {
        let Some(name) = get_str(params, "name") else {
            return err_msg(
                id,
                StandardError::InvalidParams,
                "Missing 'name' or 'command' param",
            );
        };
        let Some(cmd) = get_str(params, "command") else {
            return err_msg(
                id,
                StandardError::InvalidParams,
                "Missing 'name' or 'command' param",
            );
        };
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        match self.backend.invoke_command(name, cmd) {
            InvokeOutcome::Ok => ok(id, json!({})),
            InvokeOutcome::NotFound => err_msg(
                id,
                StandardError::InvalidParams,
                "Unknown engine or command",
            ),
            InvokeOutcome::NotSupported => err_msg(
                id,
                StandardError::MethodNotFound,
                "Command not supported by engine",
            ),
            InvokeOutcome::Failed => {
                err_msg(id, StandardError::InternalError, "engine.invoke failed")
            }
        }
    }

    fn handle_engine_load(&mut self, params: &Value, id: i64) -> Response {
        let path = get_str(params, "path").filter(|s| !s.is_empty());
        let Some(path) = path else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'path' param");
        };
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        if self.backend.engine_load(path).is_err() {
            return err_msg(id, StandardError::InternalError, "engine.load failed");
        }
        ok(id, json!({ "loaded": true, "path": path }))
    }

    fn handle_engine_unload(&mut self, params: &Value, id: i64) -> Response {
        let name = get_str(params, "name").filter(|s| !s.is_empty());
        let Some(name) = name else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'name' param");
        };
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        if self.backend.engine_unload(name).is_err() {
            return err_msg(id, StandardError::InvalidParams, "Unknown engine");
        }
        ok(id, json!({ "unloaded": true, "name": name }))
    }

    fn handle_engine_reload(&mut self, params: &Value, id: i64) -> Response {
        let name = get_str(params, "name").filter(|s| !s.is_empty());
        let Some(name) = name else {
            return err_msg(id, StandardError::InvalidParams, "Missing 'name' param");
        };
        let path = get_str(params, "path");
        if !self.backend.registry_present() {
            return err_msg(id, StandardError::InternalError, "no registry");
        }
        if self.backend.engine_reload(name, path).is_err() {
            return err_msg(id, StandardError::InternalError, "engine.reload failed");
        }
        let mut result = json!({ "reloaded": true, "name": name });
        if let Some(p) = path {
            result["path"] = Value::String(p.to_string());
        }
        ok(id, result)
    }

    // ── daemon.* ──

    fn handle_daemon_version(&self, id: i64) -> Response {
        ok(id, json!({ "version": self.backend.version() }))
    }

    fn handle_daemon_stop(&mut self, id: i64) -> Response {
        let Some(cb) = self.stop_callback.as_mut() else {
            return err_msg(id, StandardError::InternalError, "stop unavailable");
        };
        cb();
        ok(id, json!({}))
    }

    fn handle_daemon_status(&self, id: i64) -> Response {
        let active_kb = self.backend.active_keyboard();
        let active_voice = self.backend.active_voice();
        let active_lang = self.backend.active_language();
        let uptime = self.started_at.elapsed().as_secs() as i64;

        let mut result = json!({
            "version": self.backend.version(),
            "protocolVersion": protocol::PROTOCOL_VERSION,
            "uptimeSeconds": uptime,
            "activeKeyboardEngine": active_kb.unwrap_or_default(),
            "activeVoiceEngine": active_voice.unwrap_or_default(),
            "activeLanguage": active_lang.unwrap_or_default(),
        });

        if let Some(state) = self.backend.runtime_state() {
            result["runtime"] = json!({
                "frontendBackend": state.frontend_backend,
                "lifecyclePhase": state.lifecycle_phase,
                "virtualKeyboardState": state.virtual_keyboard_state,
                "keyboardGrabActive": state.keyboard_grab_active,
                "watchdogArmed": state.watchdog_armed,
            });
        }
        ok(id, result)
    }

    // ── events.subscribe ──

    fn handle_events_subscribe(
        &mut self,
        params: &Value,
        id: i64,
        client_token: ClientToken,
    ) -> Response {
        let Some(cb) = self.subscribe_callback.as_mut() else {
            return err_msg(
                id,
                StandardError::InternalError,
                "subscriptions unavailable",
            );
        };
        // `topics` is optional: absent or non-array means "subscribe to all".
        let topics: Vec<String> = params
            .get("topics")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        cb(client_token, topics);
        ok(id, json!({ "subscribed": true }))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn ok(id: i64, result: Value) -> Response {
    Response::success(Id::Number(id), result)
}

fn err(id: i64, code: StandardError, _msg: &str) -> Response {
    // Spec message; mirrors `tip_json_build_error(id, code, NULL)`-equivalent.
    Response::error(Id::Number(id), code.code(), code.message())
}

fn err_msg(id: i64, code: StandardError, msg: &str) -> Response {
    Response::error(Id::Number(id), code.code(), msg)
}

fn get_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

/// If `key` is of the form `engines.NAME.sub`, return `(NAME, sub)`.
/// Mirrors `engine_namespace()` in the C original.
fn parse_engine_namespace(key: &str) -> Option<(String, &str)> {
    let rest = key.strip_prefix("engines.")?;
    let dot = rest.find('.')?;
    if dot == 0 {
        return None;
    }
    let (name, sub) = rest.split_at(dot);
    Some((name.to_string(), &sub[1..]))
}

fn entry_to_json(e: ConfigEntry) -> Value {
    let mut obj = json!({
        "key": e.field.key,
        "type": e.field.field_type.as_str(),
        "value": e.value,
        "label": e.field.label.clone().unwrap_or_default(),
        "section": e.field.section.clone().unwrap_or_default(),
    });
    if let Some(choices) = &e.field.choices {
        obj["choices"] = Value::Array(choices.iter().map(|c| Value::String(c.clone())).collect());
    }
    obj
}

fn engine_summary(
    name: &str,
    kind: EngineKind,
    active: &Option<String>,
    display_name: Option<&str>,
) -> Value {
    let display = display_name
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string();
    let is_active = active.as_deref() == Some(name);
    json!({
        "name": name,
        "kind": kind.as_str(),
        "displayName": display,
        "active": is_active,
    })
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A scriptable fake backend. Each field doubles as the recorded call log
    /// so tests can assert on side effects (saves, notifies, invokes).
    #[derive(Default)]
    struct FakeBuilder {
        present: bool,
        keyboards: Vec<String>,
        voices: Vec<String>,
        languages: Vec<String>,
        active_kb: Option<String>,
        active_voice: Option<String>,
        active_lang: Option<String>,
        display_names: std::collections::HashMap<String, String>,
        kinds: std::collections::HashMap<String, EngineKind>,
        commands: Vec<EngineCommand>,
        config_entries: Vec<ConfigEntry>,
        config_show: String,
        config_get_unknown: bool,
        config_set_fail: bool,
        config_unset_fail: bool,
        reload_fail: bool,
        load_fail: bool,
        unload_fail: bool,
        reload_op_fail: bool,
        invoke_outcome: InvokeOutcome,
        cycle_lang_outcome: CycleLanguageOutcome,
        runtime: Option<RuntimeState>,
        version: String,
        // recorders
        saves: Rc<RefCell<u32>>,
        notifies: Rc<RefCell<Vec<(String, String, String)>>>,
    }

    impl FakeBuilder {
        fn build(self) -> Fake {
            let saves = self.saves.clone();
            let notifies = self.notifies.clone();
            Fake {
                inner: Rc::new(RefCell::new(self)),
                saves,
                notifies,
            }
        }
    }

    struct Fake {
        inner: Rc<RefCell<FakeBuilder>>,
        saves: Rc<RefCell<u32>>,
        notifies: Rc<RefCell<Vec<(String, String, String)>>>,
    }

    impl Clone for Fake {
        fn clone(&self) -> Self {
            Fake {
                inner: self.inner.clone(),
                saves: self.saves.clone(),
                notifies: self.notifies.clone(),
            }
        }
    }

    impl ServiceBackend for Fake {
        fn config_get(&self, key: &str) -> Option<ConfigGetOutcome> {
            let b = self.inner.borrow();
            if b.config_get_unknown {
                return Some(ConfigGetOutcome::Unknown);
            }
            match b.config_entries.iter().find(|e| e.field.key == key) {
                Some(entry) => Some(ConfigGetOutcome::Found {
                    value: entry.value.clone(),
                    field_type: entry.field.field_type,
                    source: entry.source,
                }),
                None => Some(ConfigGetOutcome::Unknown),
            }
        }
        fn config_set(&mut self, _key: &str, _v: &str) -> Option<Result<(), SvcError>> {
            let b = self.inner.borrow();
            if b.config_set_fail {
                Some(Err(SvcError))
            } else {
                Some(Ok(()))
            }
        }
        fn config_unset(&mut self, _key: &str) -> Option<Result<(), SvcError>> {
            let b = self.inner.borrow();
            if b.config_unset_fail {
                Some(Err(SvcError))
            } else {
                Some(Ok(()))
            }
        }
        fn config_list(&self, _prefix: &str) -> Option<Vec<ConfigEntry>> {
            let b = self.inner.borrow();
            Some(b.config_entries.clone())
        }
        fn config_show_text(&self) -> String {
            self.inner.borrow().config_show.clone()
        }
        fn config_reload(&mut self) -> Result<(), SvcError> {
            if self.inner.borrow().reload_fail {
                Err(SvcError)
            } else {
                Ok(())
            }
        }
        fn save_config(&mut self) {
            *self.saves.borrow_mut() += 1;
        }
        fn notify_engine_config(&mut self, engine: &str, key: &str, value: &str) {
            self.notifies.borrow_mut().push((
                engine.to_string(),
                key.to_string(),
                value.to_string(),
            ));
        }
        fn registry_present(&self) -> bool {
            self.inner.borrow().present
        }
        fn list_keyboards(&self) -> Vec<String> {
            self.inner.borrow().keyboards.clone()
        }
        fn list_voices(&self) -> Vec<String> {
            self.inner.borrow().voices.clone()
        }
        fn list_languages(&self) -> Vec<String> {
            self.inner.borrow().languages.clone()
        }
        fn engine_info(&self, name: &str) -> Option<EngineKind> {
            self.inner.borrow().kinds.get(name).copied()
        }
        fn engine_display_name(&self, name: &str) -> Option<String> {
            self.inner.borrow().display_names.get(name).cloned()
        }
        fn active_keyboard(&self) -> Option<String> {
            self.inner.borrow().active_kb.clone()
        }
        fn active_voice(&self) -> Option<String> {
            self.inner.borrow().active_voice.clone()
        }
        fn active_language(&self) -> Option<String> {
            self.inner.borrow().active_lang.clone()
        }
        fn set_active_keyboard(&mut self, _n: &str) -> Result<(), SvcError> {
            Ok(())
        }
        fn set_active_voice(&mut self, _n: &str) -> Result<(), SvcError> {
            Ok(())
        }
        fn set_active_language(&mut self, _t: &str) -> Result<(), SvcError> {
            Ok(())
        }
        fn cycle_keyboard(&mut self, _f: bool) -> Result<(), SvcError> {
            Ok(())
        }
        fn cycle_voice(&mut self, _f: bool) -> Result<(), SvcError> {
            Ok(())
        }
        fn cycle_language(&mut self, _f: bool) -> CycleLanguageOutcome {
            self.inner.borrow().cycle_lang_outcome.clone()
        }
        fn list_commands(&self, _n: &str) -> Vec<EngineCommand> {
            self.inner.borrow().commands.clone()
        }
        fn invoke_command(&mut self, _n: &str, _c: &str) -> InvokeOutcome {
            self.inner.borrow().invoke_outcome.clone()
        }
        fn engine_load(&mut self, _p: &str) -> Result<(), SvcError> {
            if self.inner.borrow().load_fail {
                Err(SvcError)
            } else {
                Ok(())
            }
        }
        fn engine_unload(&mut self, _n: &str) -> Result<(), SvcError> {
            if self.inner.borrow().unload_fail {
                Err(SvcError)
            } else {
                Ok(())
            }
        }
        fn engine_reload(&mut self, _n: &str, _p: Option<&str>) -> Result<(), SvcError> {
            if self.inner.borrow().reload_op_fail {
                Err(SvcError)
            } else {
                Ok(())
            }
        }
        fn version(&self) -> &str {
            // Safety of the borrow: the returned &str is owned by the Rc cell
            // and lives as long as `self`. We leak a snapshot to avoid holding
            // the RefCell borrow across the (covariant) return lifetime.
            let s = self.inner.borrow().version.clone();
            // Box-leak to obtain 'static — acceptable for a test fake.
            Box::leak(s.into_boxed_str())
        }
        fn runtime_state(&self) -> Option<RuntimeState> {
            self.inner.borrow().runtime.clone()
        }
    }

    fn fixture() -> FakeBuilder {
        let mut b = FakeBuilder {
            present: true,
            ..Default::default()
        };
        b.version = "0.0.0-test".into();
        b
    }

    fn dispatch(svc: &mut StatusService<Fake>, method: &str, params: Value) -> Response {
        svc.handle(method, &params, 7, 42)
    }

    // ── hello ──

    #[test]
    fn hello_reports_protocol_and_capabilities() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "hello", json!({}));
        let result = r.result.unwrap();
        assert_eq!(result["protocolVersion"], 3);
        assert_eq!(result["daemonVersion"], "0.0.0-test");
        let caps = result["capabilities"].as_array().unwrap();
        assert_eq!(caps.len(), 7);
        assert!(caps.iter().any(|v| v == "language"));
    }

    // ── config.* ──

    #[test]
    fn config_get_missing_key_is_invalid_params() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.get", json!({}));
        assert!(r.is_error());
        let e = r.error.unwrap();
        assert_eq!(e.code, -32602);
        assert_eq!(e.message, "Missing 'key' param");
    }

    #[test]
    fn config_get_unknown_key_is_invalid_params() {
        let mut b = fixture();
        b.config_get_unknown = true;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.get", json!({"key": "nope"}));
        assert!(r.is_error());
        assert_eq!(r.error.unwrap().message, "Unknown key");
    }

    #[test]
    fn config_get_returns_typed_value_and_source() {
        let mut b = fixture();
        b.config_entries.push(ConfigEntry {
            field: ConfigField {
                key: "theme".into(),
                field_type: FieldType::String,
                label: None,
                section: None,
                choices: None,
            },
            value: Value::String("dark".into()),
            source: ConfigSource::User,
        });
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.get", json!({"key": "theme"}));
        let result = r.result.unwrap();
        assert_eq!(result["value"], "dark");
        assert_eq!(result["type"], "string");
        assert_eq!(result["source"], "user");
    }

    #[test]
    fn config_set_string_saves_and_notifies_engine_namespace() {
        let b = fixture();
        let saves = b.saves.clone();
        let notifies = b.notifies.clone();
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(
            &mut svc,
            "config.set",
            json!({"key": "engines.rime.something", "value": "x"}),
        );
        assert!(!r.is_error());
        assert_eq!(*saves.borrow(), 1);
        let n = notifies.borrow();
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].0, "rime");
        assert_eq!(n[0].1, "engines.rime.something");
        assert_eq!(n[0].2, "x");
    }

    #[test]
    fn config_set_non_engine_key_does_not_notify() {
        let b = fixture();
        let notifies = b.notifies.clone();
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(
            &mut svc,
            "config.set",
            json!({"key": "theme", "value": "x"}),
        );
        assert!(!r.is_error());
        assert!(notifies.borrow().is_empty());
    }

    #[test]
    fn config_set_failure_is_internal_error() {
        let mut b = fixture();
        b.config_set_fail = true;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.set", json!({"key": "k", "value": "v"}));
        let e = r.error.unwrap();
        assert_eq!(e.code, -32603);
        assert_eq!(e.message, "config.set failed");
    }

    #[test]
    fn config_unset_unknown_key_is_invalid_params() {
        let mut b = fixture();
        b.config_unset_fail = true;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.unset", json!({"key": "k"}));
        assert_eq!(r.error.unwrap().message, "Unknown key");
    }

    #[test]
    fn config_list_emits_choices_when_present() {
        let mut b = fixture();
        b.config_entries.push(ConfigEntry {
            field: ConfigField {
                key: "layout".into(),
                field_type: FieldType::String,
                label: Some("Layout".into()),
                section: Some("ui".into()),
                choices: Some(vec!["qWERTY".into(), "dvorak".into()]),
            },
            value: Value::String("qWERTY".into()),
            source: ConfigSource::Default,
        });
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.list", json!({"prefix": ""}));
        let arr = r.result.unwrap().as_array().unwrap().clone();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["choices"].as_array().unwrap().len(), 2);
        assert_eq!(arr[0]["section"], "ui");
        // config.list emits field/type/value/label/section (+choices); it does
        // not surface the user-vs-default source the way config.get does.
        assert_eq!(arr[0]["type"], "string");
        assert_eq!(arr[0]["value"], "qWERTY");
    }

    #[test]
    fn config_show_returns_toml_format() {
        let mut b = fixture();
        b.config_show = "theme = \"dark\"".into();
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.show", json!({}));
        let result = r.result.unwrap();
        assert_eq!(result["format"], "toml");
        assert_eq!(result["text"], "theme = \"dark\"");
    }

    #[test]
    fn config_reload_failure_is_internal_error() {
        let mut b = fixture();
        b.reload_fail = true;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "config.reload", json!({}));
        assert_eq!(r.error.unwrap().message, "config.reload failed");
    }

    // ── engine.* ──

    #[test]
    fn engine_list_merges_keyboards_then_voices_with_active_flags() {
        let mut b = fixture();
        b.keyboards = vec!["basic".into(), "rime".into()];
        b.voices = vec!["whisper".into()];
        b.active_kb = Some("rime".into());
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "engine.list", json!({}));
        let arr = r.result.unwrap().as_array().unwrap().clone();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["name"], "basic");
        assert_eq!(arr[0]["kind"], "keyboard");
        assert_eq!(arr[0]["active"], false);
        assert_eq!(arr[1]["name"], "rime");
        assert_eq!(arr[1]["active"], true);
        assert_eq!(arr[2]["kind"], "voice");
    }

    #[test]
    fn engine_describe_unknown_engine_is_invalid_params() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "engine.describe", json!({"name": "ghost"}));
        assert_eq!(r.error.unwrap().message, "Unknown engine");
    }

    #[test]
    fn engine_describe_returns_properties_and_commands() {
        let mut b = fixture();
        b.kinds.insert("rime".into(), EngineKind::Keyboard);
        b.display_names.insert("rime".into(), "Rime".into());
        b.commands = vec![EngineCommand {
            id: "sync".into(),
            label: "Sync".into(),
        }];
        b.config_entries.push(ConfigEntry {
            field: ConfigField {
                key: "engines.rime.something".into(),
                field_type: FieldType::Int,
                label: None,
                section: None,
                choices: None,
            },
            value: Value::Number(5.into()),
            source: ConfigSource::User,
        });
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "engine.describe", json!({"name": "rime"}));
        let result = r.result.unwrap();
        assert_eq!(result["displayName"], "Rime");
        assert_eq!(result["properties"].as_array().unwrap().len(), 1);
        assert_eq!(result["commands"][0]["id"], "sync");
    }

    #[test]
    fn keyboard_use_on_voice_engine_is_invalid_params() {
        let mut b = fixture();
        b.kinds.insert("whisper".into(), EngineKind::Voice);
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "keyboard.use", json!({"name": "whisper"}));
        assert_eq!(r.error.unwrap().message, "Not a keyboard engine");
    }

    #[test]
    fn voice_use_on_keyboard_engine_is_invalid_params() {
        let mut b = fixture();
        b.kinds.insert("basic".into(), EngineKind::Keyboard);
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "voice.use", json!({"name": "basic"}));
        assert_eq!(r.error.unwrap().message, "Not a voice engine");
    }

    #[test]
    fn keyboard_next_returns_active_name() {
        let mut b = fixture();
        b.active_kb = Some("rime".into());
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "keyboard.next", json!({}));
        assert_eq!(r.result.unwrap()["active"], "rime");
    }

    #[test]
    fn no_registry_is_internal_error_for_engine_list() {
        let mut b = fixture();
        b.present = false;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "engine.list", json!({}));
        assert_eq!(r.error.unwrap().message, "no registry");
    }

    // ── language.* ──

    #[test]
    fn language_list_marks_active_tag() {
        let mut b = fixture();
        b.languages = vec!["en".into(), "ja".into()];
        b.active_lang = Some("ja".into());
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "language.list", json!({}));
        let result = r.result.unwrap();
        let langs = result["languages"].as_array().unwrap();
        assert_eq!(langs[0]["active"], false);
        assert_eq!(langs[1]["active"], true);
        assert_eq!(result["active"], "ja");
    }

    #[test]
    fn language_cycle_no_languages_is_invalid_params() {
        let mut b = fixture();
        b.cycle_lang_outcome = CycleLanguageOutcome::NoLanguages;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "language.next", json!({}));
        assert_eq!(r.error.unwrap().message, "No languages enabled or declared");
    }

    // ── engine.invoke / load / unload / reload ──

    #[test]
    fn engine_invoke_not_supported_is_method_not_found() {
        let mut b = fixture();
        b.invoke_outcome = InvokeOutcome::NotSupported;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(
            &mut svc,
            "engine.invoke",
            json!({"name": "x", "command": "c"}),
        );
        let e = r.error.unwrap();
        assert_eq!(e.code, -32601);
        assert_eq!(e.message, "Command not supported by engine");
    }

    #[test]
    fn engine_invoke_not_found_is_invalid_params() {
        let mut b = fixture();
        b.invoke_outcome = InvokeOutcome::NotFound;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(
            &mut svc,
            "engine.invoke",
            json!({"name": "x", "command": "c"}),
        );
        assert_eq!(r.error.unwrap().message, "Unknown engine or command");
    }

    #[test]
    fn engine_load_failure_is_internal_error() {
        let mut b = fixture();
        b.load_fail = true;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "engine.load", json!({"path": "/x.so"}));
        assert_eq!(r.error.unwrap().message, "engine.load failed");
    }

    #[test]
    fn engine_unload_unknown_is_invalid_params() {
        let mut b = fixture();
        b.unload_fail = true;
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "engine.unload", json!({"name": "x"}));
        assert_eq!(r.error.unwrap().message, "Unknown engine");
    }

    #[test]
    fn engine_reload_succeeds_with_optional_path() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(
            &mut svc,
            "engine.reload",
            json!({"name": "rime", "path": "/p"}),
        );
        let result = r.result.unwrap();
        assert_eq!(result["reloaded"], true);
        assert_eq!(result["path"], "/p");
    }

    // ── daemon.* ──

    #[test]
    fn daemon_version_returns_backend_version() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "daemon.version", json!({}));
        assert_eq!(r.result.unwrap()["version"], "0.0.0-test");
    }

    #[test]
    fn daemon_stop_without_callback_is_internal_error() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "daemon.stop", json!({}));
        assert_eq!(r.error.unwrap().message, "stop unavailable");
    }

    #[test]
    fn daemon_stop_with_callback_succeeds() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let stopped = Rc::new(RefCell::new(false));
        let s = stopped.clone();
        svc.set_stop_callback(move || *s.borrow_mut() = true);
        let r = dispatch(&mut svc, "daemon.stop", json!({}));
        assert!(!r.is_error());
        assert!(*stopped.borrow());
    }

    #[test]
    fn daemon_status_includes_runtime_when_provided() {
        let mut b = fixture();
        let mut rt = RuntimeState {
            frontend_backend: "input-method-v2".into(),
            ..Default::default()
        };
        rt.lifecycle_phase = "running".into();
        rt.keyboard_grab_active = true;
        rt.watchdog_armed = false;
        b.runtime = Some(rt);
        let fake = b.build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "daemon.status", json!({}));
        let result = r.result.unwrap();
        assert_eq!(result["runtime"]["frontendBackend"], "input-method-v2");
        assert_eq!(result["runtime"]["lifecyclePhase"], "running");
        assert_eq!(result["runtime"]["keyboardGrabActive"], true);
    }

    #[test]
    fn daemon_status_omits_runtime_when_absent() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "daemon.status", json!({}));
        let result = r.result.unwrap();
        assert!(result.get("runtime").is_none());
        assert_eq!(result["protocolVersion"], 3);
    }

    // ── events.subscribe ──

    #[test]
    fn events_subscribe_without_callback_is_internal_error() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "events.subscribe", json!({"topics": ["engine"]}));
        assert_eq!(r.error.unwrap().message, "subscriptions unavailable");
    }

    #[test]
    fn events_subscribe_forwards_topics_and_token() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let received = Rc::new(RefCell::new((0u64, Vec::new())));
        let r = received.clone();
        svc.set_subscribe_callback(move |token, topics| {
            r.borrow_mut().0 = token;
            r.borrow_mut().1 = topics;
        });
        let resp = svc.handle(
            "events.subscribe",
            &json!({"topics": ["engine", "language"]}),
            9,
            99,
        );
        assert!(!resp.is_error());
        assert_eq!(resp.result.unwrap()["subscribed"], true);
        assert_eq!(received.borrow().0, 99);
        assert_eq!(received.borrow().1, vec!["engine", "language"]);
    }

    #[test]
    fn events_subscribe_without_topics_means_all() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let received = Rc::new(RefCell::new(Vec::new()));
        let r = received.clone();
        svc.set_subscribe_callback(move |_t, topics| *r.borrow_mut() = topics);
        let r2 = dispatch(&mut svc, "events.subscribe", json!({}));
        assert!(!r2.is_error());
        assert!(received.borrow().is_empty());
    }

    // ── top-level routing ──

    #[test]
    fn unknown_method_is_method_not_found() {
        let fake = fixture().build();
        let mut svc = StatusService::new(fake);
        let r = dispatch(&mut svc, "bogus.method", json!({}));
        assert_eq!(r.error.unwrap().code, -32601);
    }

    // ── pure helpers ──

    #[test]
    fn parse_engine_namespace_splits_name_and_subkey() {
        let (name, sub) = parse_engine_namespace("engines.rime.something").unwrap();
        assert_eq!(name, "rime");
        assert_eq!(sub, "something");
    }

    #[test]
    fn parse_engine_namespace_rejects_non_engine_keys() {
        assert!(parse_engine_namespace("theme").is_none());
        assert!(parse_engine_namespace("engines.rime").is_none()); // no subkey
    }

    #[test]
    fn field_type_coercion_matches_c_semantics() {
        assert_eq!(FieldType::Int.coerce_from_str("42"), json!(42));
        assert_eq!(FieldType::Int.coerce_from_str("garbage"), json!(0)); // strtol → 0
        assert!(FieldType::Bool.coerce_from_str("true") == json!(true));
        assert!(FieldType::Bool.coerce_from_str("1") == json!(true));
        assert!(FieldType::Bool.coerce_from_str("0") == json!(false));
        assert_eq!(FieldType::String.coerce_from_str("x"), json!("x"));
    }
}
