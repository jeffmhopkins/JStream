//! The JSON-RPC 2.0 subset MCP uses: parsing one incoming message and building replies.
//!
//! MCP narrows JSON-RPC: request IDs are strings or integers (never `null`), `params` is
//! an object, and there are no batches (MCP removed them in 2025-06-18). Anything else is
//! answered with the standard error codes (`-32700`, `-32600`..`-32603`).

use serde_json::{Map, Value, json};

/// Invalid JSON.
pub(crate) const PARSE_ERROR: i64 = -32700;
/// Valid JSON, but not a valid request object.
pub(crate) const INVALID_REQUEST: i64 = -32600;
/// The method doesn't exist here.
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
/// The params don't fit the method.
pub(crate) const INVALID_PARAMS: i64 = -32602;
/// MCP 2026-07-28: the request's protocol version is not served (`UnsupportedProtocolVersion`).
pub(crate) const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// A JSON-RPC error object: what the reply's `error` member holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RpcError {
    pub(crate) code: i64,
    pub(crate) message: String,
    pub(crate) data: Option<Value>,
}

impl RpcError {
    pub(crate) fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        RpcError::new(INVALID_PARAMS, message)
    }

    fn to_value(&self) -> Value {
        let mut error = json!({"code": self.code, "message": self.message});
        if let (Some(data), Some(object)) = (&self.data, error.as_object_mut()) {
            object.insert("data".into(), data.clone());
        }
        error
    }
}

/// One parsed incoming message.
#[derive(Debug, PartialEq)]
pub(crate) enum Incoming {
    /// A request: it gets exactly one reply.
    Request {
        /// A string or an integer, echoed in the reply.
        id: Value,
        method: String,
        /// Empty when the request had no `params`.
        params: Map<String, Value>,
    },
    /// A notification (no `id`): never answered.
    Notification,
    /// A response. Clients never send them to a server, so it is dropped unanswered.
    Response,
}

/// Parses one message. On failure, returns the error reply to send.
pub(crate) fn parse(text: &str) -> Result<Incoming, Value> {
    let value: Value = serde_json::from_str(text).map_err(|e| {
        error_reply(
            None,
            &RpcError::new(PARSE_ERROR, format!("parse error: {e}")),
        )
    })?;
    let object = match value {
        Value::Object(object) => object,
        Value::Array(_) => {
            return Err(error_reply(
                None,
                &RpcError::new(INVALID_REQUEST, "JSON-RPC batches are not supported by MCP"),
            ));
        }
        _ => {
            return Err(error_reply(
                None,
                &RpcError::new(INVALID_REQUEST, "a message must be a JSON object"),
            ));
        }
    };
    let id = match object.get("id") {
        None => None,
        Some(id) if is_valid_id(id) => Some(id.clone()),
        Some(_) => {
            return Err(error_reply(
                None,
                &RpcError::new(INVALID_REQUEST, "`id` must be a string or an integer"),
            ));
        }
    };
    let invalid =
        |message: &str| error_reply(id.as_ref(), &RpcError::new(INVALID_REQUEST, message));
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(invalid("`jsonrpc` must be \"2.0\""));
    }
    let method = match object.get("method") {
        Some(Value::String(method)) => method.clone(),
        Some(_) => return Err(invalid("`method` must be a string")),
        None if object.contains_key("result") || object.contains_key("error") => {
            return Ok(Incoming::Response);
        }
        None => return Err(invalid("`method` is missing")),
    };
    let Some(id) = id else {
        return Ok(Incoming::Notification);
    };
    let params = match object.get("params") {
        None => Map::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(_) => {
            return Err(error_reply(
                Some(&id),
                &RpcError::invalid_params("`params` must be an object"),
            ));
        }
    };
    Ok(Incoming::Request { id, method, params })
}

/// MCP request IDs: a string or an integer, never `null`.
fn is_valid_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

/// A success reply.
pub(crate) fn result_reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// An error reply. `id` is `None` when the request's ID could not be read; JSON-RPC then
/// sends `null`.
pub(crate) fn error_reply(id: Option<&Value>, error: &RpcError) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": error.to_value()})
}

#[cfg(test)]
mod tests {
    use super::{Incoming, parse};
    use serde_json::{Value, json};

    fn error_code(reply: &Value) -> Option<i64> {
        reply["error"]["code"].as_i64()
    }

    #[test]
    fn requests_notifications_and_responses() {
        let parsed = parse(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#);
        assert_eq!(
            parsed,
            Ok(Incoming::Request {
                id: json!(7),
                method: "tools/list".into(),
                params: serde_json::Map::new(),
            })
        );
        let parsed = parse(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert_eq!(parsed, Ok(Incoming::Notification));
        let parsed = parse(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#);
        assert_eq!(parsed, Ok(Incoming::Response));
    }

    #[test]
    fn malformed_messages_get_standard_codes() {
        let cases = [
            ("{not json", -32700, Value::Null),
            ("[]", -32600, Value::Null),
            (
                r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
                -32600,
                Value::Null,
            ),
            ("42", -32600, Value::Null),
            (r#"{"id":1,"method":"ping"}"#, -32600, json!(1)),
            (
                r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
                -32600,
                json!(1),
            ),
            (
                r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
                -32600,
                Value::Null,
            ),
            (
                r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#,
                -32600,
                Value::Null,
            ),
            (
                r#"{"jsonrpc":"2.0","id":true,"method":"ping"}"#,
                -32600,
                Value::Null,
            ),
            (r#"{"jsonrpc":"2.0","id":"x"}"#, -32600, json!("x")),
            (
                r#"{"jsonrpc":"2.0","id":"x","method":3}"#,
                -32600,
                json!("x"),
            ),
            (
                r#"{"jsonrpc":"2.0","id":2,"method":"ping","params":[1]}"#,
                -32602,
                json!(2),
            ),
        ];
        for (text, code, id) in cases {
            let reply = parse(text).expect_err(text);
            assert_eq!(error_code(&reply), Some(code), "{text}");
            assert_eq!(reply["id"], id, "{text}");
            assert_eq!(reply["jsonrpc"], "2.0", "{text}");
            assert!(reply["error"]["message"].is_string(), "{text}");
        }
    }
}
