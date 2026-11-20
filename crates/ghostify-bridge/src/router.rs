//! Method registry of the bridge and the built-in methods.

use crate::protocol::{RpcError, METHOD_NOT_FOUND};
use ghostify_db::{repo::settings, Db};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;
type Handler = Arc<dyn Fn(CallContext, Value) -> HandlerFuture + Send + Sync>;

/// What a handler knows about the caller.
#[derive(Debug, Clone, Copy)]
pub struct CallContext {
    pub session_id: u64,
}

/// Largest value one `storage.set` call may store.
pub const MAX_STORAGE_VALUE_BYTES: usize = 256 * 1024;
const MAX_NAME_LEN: usize = 64;

#[derive(Default, Clone)]
pub struct Router {
    handlers: HashMap<String, Handler>,
}

impl Router {
    pub fn new() -> Router {
        Router::default()
    }

    /// Router with `ping`, `session.hello` and `storage.get/set/delete`.
    pub fn with_builtin(db: Db) -> Router {
        let mut router = Router::new();
        router.register("ping", |_, _| async { Ok(json!("pong")) });
        router.register("session.hello", |ctx, params| async move {
            #[derive(Deserialize)]
            struct Hello {
                client: String,
                version: String,
            }
            let hello: Hello = parse(params)?;
            if hello.client.len() > 128 || hello.version.len() > 128 {
                return Err(RpcError::invalid_params("client and version must be short"));
            }
            Ok(json!({
                "server": "ghostify",
                "version": env!("CARGO_PKG_VERSION"),
                "session_id": ctx.session_id,
            }))
        });

        #[derive(Deserialize)]
        struct Slot {
            namespace: String,
            key: String,
        }
        fn slot_key(slot: &Slot) -> Result<String, RpcError> {
            for (label, value) in [("namespace", &slot.namespace), ("key", &slot.key)] {
                let valid = !value.is_empty()
                    && value.len() <= MAX_NAME_LEN
                    && value.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '@')
                    });
                if !valid {
                    return Err(RpcError::invalid_params(format!(
                        "{label} must be 1-{MAX_NAME_LEN} characters of letters, digits and . _ - / @"
                    )));
                }
            }
            Ok(format!("plugin-storage:{}:{}", slot.namespace, slot.key))
        }

        let get_db = db.clone();
        router.register("storage.get", move |_, params| {
            let db = get_db.clone();
            async move {
                let key = slot_key(&parse(params)?)?;
                let value: Option<Value> = db.call(move |c| settings::get(c, &key)).await?;
                Ok(value.unwrap_or(Value::Null))
            }
        });

        let set_db = db.clone();
        router.register("storage.set", move |_, params| {
            let db = set_db.clone();
            async move {
                #[derive(Deserialize)]
                struct SetParams {
                    namespace: String,
                    key: String,
                    value: Value,
                }
                let set: SetParams = parse(params)?;
                let key = slot_key(&Slot {
                    namespace: set.namespace,
                    key: set.key,
                })?;
                let size = serde_json::to_vec(&set.value)
                    .map(|v| v.len())
                    .unwrap_or(usize::MAX);
                if size > MAX_STORAGE_VALUE_BYTES {
                    return Err(RpcError::invalid_params(format!(
                        "value is larger than {MAX_STORAGE_VALUE_BYTES} bytes"
                    )));
                }
                let value = set.value;
                db.call(move |c| settings::set(c, &key, &value)).await?;
                Ok(Value::Null)
            }
        });

        let delete_db = db;
        router.register("storage.delete", move |_, params| {
            let db = delete_db.clone();
            async move {
                let key = slot_key(&parse(params)?)?;
                let existed = db.call(move |c| settings::delete(c, &key)).await?;
                Ok(json!(existed))
            }
        });
        router
    }

    /// Adds or replaces a method.
    pub fn register<F, Fut>(&mut self, method: &str, handler: F)
    where
        F: Fn(CallContext, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, RpcError>> + Send + 'static,
    {
        self.handlers.insert(
            method.to_owned(),
            Arc::new(move |ctx, params| Box::pin(handler(ctx, params))),
        );
    }

    pub fn has_method(&self, method: &str) -> bool {
        self.handlers.contains_key(method)
    }

    pub async fn dispatch(
        &self,
        ctx: CallContext,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        match self.handlers.get(method) {
            Some(handler) => handler(ctx, params).await,
            None => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method `{method}`"),
            )),
        }
    }
}

/// Deserialises parameters; a client error becomes `INVALID_PARAMS`.
pub fn parse<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::invalid_params(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{INVALID_PARAMS, NOT_FOUND};

    const CTX: CallContext = CallContext { session_id: 7 };

    fn router() -> Router {
        Router::with_builtin(Db::open_in_memory().unwrap())
    }

    #[tokio::test]
    async fn ping_and_hello() {
        let router = router();
        assert_eq!(
            router.dispatch(CTX, "ping", Value::Null).await.unwrap(),
            json!("pong")
        );
        let hello = router
            .dispatch(
                CTX,
                "session.hello",
                json!({"client": "loader", "version": "1"}),
            )
            .await
            .unwrap();
        assert_eq!(hello["server"], "ghostify");
        assert_eq!(hello["session_id"], 7);
        let err = router
            .dispatch(CTX, "session.hello", json!({"client": 1}))
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn unknown_method_is_reported() {
        let err = router()
            .dispatch(CTX, "nope", Value::Null)
            .await
            .unwrap_err();
        assert_eq!(err.code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn storage_round_trip_is_namespaced() {
        let router = router();
        let slot = |ns: &str| json!({"namespace": ns, "key": "settings"});
        assert_eq!(
            router
                .dispatch(CTX, "storage.get", slot("a/one"))
                .await
                .unwrap(),
            Value::Null
        );

        router
            .dispatch(
                CTX,
                "storage.set",
                json!({"namespace": "a/one", "key": "settings", "value": {"x": [1, 2]}}),
            )
            .await
            .unwrap();
        assert_eq!(
            router
                .dispatch(CTX, "storage.get", slot("a/one"))
                .await
                .unwrap(),
            json!({"x": [1, 2]})
        );
        assert_eq!(
            router
                .dispatch(CTX, "storage.get", slot("b/two"))
                .await
                .unwrap(),
            Value::Null
        );

        assert_eq!(
            router
                .dispatch(CTX, "storage.delete", slot("a/one"))
                .await
                .unwrap(),
            json!(true)
        );
        assert_eq!(
            router
                .dispatch(CTX, "storage.delete", slot("a/one"))
                .await
                .unwrap(),
            json!(false)
        );
    }

    #[tokio::test]
    async fn storage_validates_names_and_sizes() {
        let router = router();
        for bad in ["", "has space", "semi;colon", &"x".repeat(65)] {
            let err = router
                .dispatch(CTX, "storage.get", json!({"namespace": bad, "key": "k"}))
                .await
                .unwrap_err();
            assert_eq!(err.code, INVALID_PARAMS, "namespace `{bad}`");
        }
        let huge = "x".repeat(MAX_STORAGE_VALUE_BYTES + 1);
        let err = router
            .dispatch(
                CTX,
                "storage.set",
                json!({"namespace": "a", "key": "k", "value": huge}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert_ne!(err.code, NOT_FOUND);
    }

    #[tokio::test]
    async fn custom_methods_can_be_registered_and_replaced() {
        let mut router = router();
        router.register("echo", |_, params| async move { Ok(params) });
        assert_eq!(
            router.dispatch(CTX, "echo", json!([1])).await.unwrap(),
            json!([1])
        );
        router.register("echo", |_, _| async { Ok(json!("replaced")) });
        assert_eq!(
            router.dispatch(CTX, "echo", Value::Null).await.unwrap(),
            json!("replaced")
        );
        assert!(router.has_method("echo") && !router.has_method("missing"));
    }
}
