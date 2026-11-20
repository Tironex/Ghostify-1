//! JSON-RPC 2.0 message types used on the bridge.

use ghostify_core::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
/// Application errors live in the range reserved by the specification.
pub const UNAUTHORIZED: i32 = -32001;
pub const PERMISSION_DENIED: i32 = -32002;
pub const NOT_FOUND: i32 = -32004;
pub const RATE_LIMITED: i32 = -32005;
pub const CONFLICT: i32 = -32009;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> RpcError {
        RpcError::new(INVALID_PARAMS, message)
    }
}

impl From<Error> for RpcError {
    fn from(err: Error) -> RpcError {
        let code = match &err {
            Error::Invalid(_) | Error::Serde(_) => INVALID_PARAMS,
            Error::NotFound(_) => NOT_FOUND,
            Error::PermissionDenied(_) => PERMISSION_DENIED,
            Error::Conflict(_) => CONFLICT,
            _ => INTERNAL_ERROR,
        };
        // Internal details (paths, SQL) are logged, not sent to the renderer.
        let message = if code == INTERNAL_ERROR {
            tracing::error!(%err, "internal error in bridge handler");
            "internal error".to_owned()
        } else {
            err.to_string()
        };
        RpcError {
            code,
            message,
            data: Some(serde_json::json!({ "kind": err.code() })),
        }
    }
}

/// A request or notification from the client. A message without `id` is a
/// notification and gets no response.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub id: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Response {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: RpcError) -> Response {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}}"#
                .to_owned()
        })
    }
}

/// Server push without a response.
#[derive(Debug, Clone, Serialize)]
pub struct Notification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    pub params: &'a Value,
}

pub fn notification_text(method: &str, params: &Value) -> String {
    serde_json::to_string(&Notification {
        jsonrpc: "2.0",
        method,
        params,
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_map_to_codes_and_hide_internals() {
        let invalid: RpcError = Error::Invalid("bad".into()).into();
        assert_eq!(invalid.code, INVALID_PARAMS);
        assert_eq!(invalid.data.unwrap()["kind"], "invalid_input");

        let missing: RpcError = Error::NotFound("x".into()).into();
        assert_eq!(missing.code, NOT_FOUND);

        let internal: RpcError = Error::Database("secret path /home/user/db".into()).into();
        assert_eq!(internal.code, INTERNAL_ERROR);
        assert_eq!(internal.message, "internal error");
    }

    #[test]
    fn response_serialises_one_of_result_or_error() {
        let ok = serde_json::to_value(Response::ok(Value::from(1), Value::from("pong"))).unwrap();
        assert_eq!(ok["result"], "pong");
        assert!(ok.get("error").is_none());
        let err = serde_json::to_value(Response::err(Value::Null, RpcError::new(PARSE_ERROR, "x")))
            .unwrap();
        assert_eq!(err["error"]["code"], PARSE_ERROR);
        assert!(err.get("result").is_none());
    }

    #[test]
    fn request_without_id_is_a_notification() {
        let r: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"x"}"#).unwrap();
        assert!(r.id.is_none());
        assert_eq!(r.params, Value::Null);
    }
}
