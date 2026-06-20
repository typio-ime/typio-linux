//! JSON-RPC 2.0 envelope types for TIP v3.
//!
//! Port of the high-level framing helpers in `src/ipc/tip_json.{h,c}`
//! (`tip_json_build_response`, `tip_json_build_error`,
//! `tip_json_build_notify`). The C version rolls its own JSON builder for
//! these because it has no external JSON library; the Rust port uses
//! `serde_json` and `serde` derives, eliminating the hand-rolled
//! serialisation entirely.
//!
//! ## What this module does NOT define
//!
//! Per-method typed `params` / `result` structs (e.g. `HelloParams`,
//! `ConfigGetResult`). The C daemon dispatches per-method using ad-hoc
//! JSON values; per-method typed structs will land alongside the
//! corresponding handler port (config access, engine registry, etc.).
//! Until then, `params` and `result` are untyped
//! [`serde_json::Value`]s — exactly what the C version uses.
//!
//! ## JSON-RPC 2.0 in one paragraph
//!
//! Three message kinds share the wire:
//!
//! - **Request**: `{jsonrpc, id, method, params?}` — client wants the
//!   daemon to do something and waits for a response.
//! - **Response**: `{jsonrpc, id, result}` (success) or
//!   `{jsonrpc, id, error: {code, message, data?}}` (failure) — daemon's
//!   answer to a Request. The `id` echoes the Request's.
//! - **Notification**: `{jsonrpc, method, params?}` — one-way push from
//!   daemon to subscribed client (no `id`, no response expected).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::JSONRPC_VERSION;

/// JSON-RPC id. Per spec, may be a number, a string, or null (for
/// notifications). The TIP layer uses integer ids exclusively in
/// practice, but we round-trip the full spec set to avoid silently
/// corrupting non-conforming clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// Numeric id (the common case).
    Number(i64),
    /// String id.
    String(String),
    /// Explicit null. Distinguishable from absent (a Notification has
    /// no `id` field at all; a Response to a Notification-shaped Request
    /// would carry `id: null`).
    Null,
}

impl From<i64> for Id {
    fn from(n: i64) -> Self {
        Id::Number(n)
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Id::String(s.to_string())
    }
}

/// Top-level message kinds. Use [`Message::parse`] to decode an incoming
/// JSON string; the untagged enum resolves to the right variant based on
/// which fields are present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    /// Client-to-daemon request, expects a response.
    Request(Request),
    /// Daemon-to-client response.
    Response(Response),
    /// One-way daemon-to-client push.
    Notification(Notification),
}

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`. Not parsed strictly to allow lenient interop with
    /// clients that omit the field; serialise always.
    pub jsonrpc: String,
    /// Client-supplied id, echoed in the response.
    pub id: Id,
    /// Method name — one of [`super::protocol::methods`].
    pub method: String,
    /// Method arguments. Absent if the method takes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response (success or failure).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    /// Echoes the request id.
    pub id: Id,
    /// Present on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Response {
    /// Convenience: build a success response with the given result value.
    pub fn success(id: impl Into<Id>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    /// Convenience: build an error response.
    pub fn error(id: impl Into<Id>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            result: None,
            error: Some(Error {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// True iff this response carries an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    /// Numeric error code. Codes from -32768 to -32000 are reserved by
    /// the spec; applications use codes outside that range.
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard JSON-RPC error codes.
///
/// These are part of the spec; daemon responses should use them where
/// applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardError {
    /// Invalid JSON.
    ParseError,
    /// Valid JSON but not a valid Request object.
    InvalidRequest,
    /// Method name not recognised.
    MethodNotFound,
    /// Method parameters invalid for the named method.
    InvalidParams,
    /// Internal error.
    InternalError,
}

impl StandardError {
    /// Spec-defined numeric code.
    pub fn code(self) -> i32 {
        match self {
            StandardError::ParseError => -32700,
            StandardError::InvalidRequest => -32600,
            StandardError::MethodNotFound => -32601,
            StandardError::InvalidParams => -32602,
            StandardError::InternalError => -32603,
        }
    }

    /// Spec-defined message string.
    pub fn message(self) -> &'static str {
        match self {
            StandardError::ParseError => "Parse error",
            StandardError::InvalidRequest => "Invalid Request",
            StandardError::MethodNotFound => "Method not found",
            StandardError::InvalidParams => "Invalid params",
            StandardError::InternalError => "Internal error",
        }
    }
}

/// JSON-RPC 2.0 notification (no id, no response expected).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    /// Topic name — one of [`super::protocol::topics`].
    pub method: String,
    /// Topic payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Convenience: build a notification with the given method + params.
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params: Some(params),
        }
    }
}

impl Message {
    /// Parse a JSON string into a typed message.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialise to a JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Request {
    /// Convenience: construct a new request with the given id + method.
    /// `params` defaults to None.
    pub fn new(id: impl Into<Id>, method: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.into(),
            method: method.into(),
            params: None,
        }
    }

    /// Attach params to the request (builder pattern).
    pub fn with_params(mut self, params: Value) -> Self {
        self.params = Some(params);
        self
    }

    /// Parse a JSON string into a request.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialise to a JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Response {
    /// Parse a JSON string into a response.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialise to a JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Notification {
    /// Parse a JSON string into a notification.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialise to a JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip_with_params() {
        let req = Request::new(42, "config.get").with_params(json!({"key": "voice.engine"}));
        let serialized = req.to_json().unwrap();
        assert!(serialized.contains("\"jsonrpc\":\"2.0\""));
        assert!(serialized.contains("\"id\":42"));
        assert!(serialized.contains("\"method\":\"config.get\""));
        assert!(serialized.contains("\"params\":"));

        // Round-trip back through the untagged Message enum.
        let parsed = Message::parse(&serialized).unwrap();
        match parsed {
            Message::Request(r) => {
                assert_eq!(r.id, Id::Number(42));
                assert_eq!(r.method, "config.get");
                assert_eq!(r.params, Some(json!({"key": "voice.engine"})));
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[test]
    fn request_without_params_omits_field() {
        let req = Request::new(7, "engine.list");
        let serialized = req.to_json().unwrap();
        // skip_serializing_if ensures absent params stays out of the wire.
        assert!(!serialized.contains("params"));
    }

    #[test]
    fn string_id_round_trips() {
        let req = Request::new("abc-123", "hello");
        let json_str = req.to_json().unwrap();
        assert!(json_str.contains("\"id\":\"abc-123\""));
        let parsed = Request::parse(&json_str).unwrap();
        assert_eq!(parsed.id, Id::String("abc-123".to_string()));
    }

    #[test]
    fn success_response_round_trips() {
        let resp = Response::success(99, json!({"protocolVersion": 3}));
        let s = resp.to_json().unwrap();
        assert!(s.contains("\"result\":"));
        assert!(!s.contains("\"error\""));
        assert!(!resp.is_error());

        let parsed = Response::parse(&s).unwrap();
        assert_eq!(parsed.id, Id::Number(99));
        assert_eq!(parsed.result, Some(json!({"protocolVersion": 3})));
        assert!(parsed.error.is_none());
    }

    #[test]
    fn error_response_round_trips() {
        let resp = Response::error(99, -32601, "Method not found");
        let s = resp.to_json().unwrap();
        assert!(s.contains("\"error\""));
        assert!(s.contains("\"code\":-32601"));
        assert!(s.contains("\"message\":\"Method not found\""));
        assert!(!s.contains("\"result\""));
        assert!(resp.is_error());
    }

    #[test]
    fn notification_round_trips() {
        let n = Notification::new("engine.changed", json!({"name": "rime"}));
        let s = n.to_json().unwrap();
        // No id field on a notification.
        assert!(!s.contains("\"id\""));
        assert!(s.contains("\"method\":\"engine.changed\""));
        assert!(s.contains("\"params\":"));

        let parsed = Notification::parse(&s).unwrap();
        assert_eq!(parsed.method, "engine.changed");
        assert_eq!(parsed.params, Some(json!({"name": "rime"})));
    }

    #[test]
    fn message_parse_distinguishes_kinds() {
        // Request → has method + id, no result/error.
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"hello"}"#;
        assert!(matches!(Message::parse(req).unwrap(), Message::Request(_)));
        // Response (success) → has id + result.
        let resp = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
        assert!(matches!(Message::parse(resp).unwrap(), Message::Response(_)));
        // Notification → has method, no id.
        let notif = r#"{"jsonrpc":"2.0","method":"engine.changed","params":{}}"#;
        assert!(matches!(
            Message::parse(notif).unwrap(),
            Message::Notification(_)
        ));
    }

    #[test]
    fn standard_error_codes_match_spec() {
        assert_eq!(StandardError::ParseError.code(), -32700);
        assert_eq!(StandardError::InvalidRequest.code(), -32600);
        assert_eq!(StandardError::MethodNotFound.code(), -32601);
        assert_eq!(StandardError::InvalidParams.code(), -32602);
        assert_eq!(StandardError::InternalError.code(), -32603);

        assert_eq!(StandardError::ParseError.message(), "Parse error");
        assert_eq!(StandardError::MethodNotFound.message(), "Method not found");
    }

    #[test]
    fn id_construction_helpers() {
        assert_eq!(Id::from(42), Id::Number(42));
        assert_eq!(Id::from("x"), Id::String("x".to_string()));
    }

    #[test]
    fn real_world_hello_request_parses() {
        // Sample of what typioctl actually sends on connect.
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"typioctl","clientVersion":"0.1.1"}}"#;
        let parsed = Request::parse(raw).unwrap();
        assert_eq!(parsed.method, "hello");
        assert_eq!(parsed.id, Id::Number(1));
        let params = parsed.params.unwrap();
        assert_eq!(params["client"], json!("typioctl"));
    }

    #[test]
    fn real_world_hello_response_serializes() {
        // What the daemon should send back to typioctl.
        let resp = Response::success(
            1,
            json!({
                "protocolVersion": 3,
                "daemonVersion": env!("CARGO_PKG_VERSION"),
                "daemon": "typio",
            }),
        );
        let s = resp.to_json().unwrap();
        // Verify the wire form typioctl expects.
        assert!(s.contains("\"protocolVersion\":3"));
    }
}
