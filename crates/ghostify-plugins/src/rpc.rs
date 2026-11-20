//! Bridge methods used by the plugin host inside Spotify.

use crate::manifest::Permission;
use crate::store::PluginStore;
use ghostify_bridge::router::parse;
use ghostify_bridge::{Router, RpcError};
use ghostify_core::{Error, PluginState, Result};
use ghostify_db::repo::plugins::PluginEventKind;
use serde::Deserialize;
use serde_json::{json, Value};

async fn blocking<T: Send + 'static>(
    store: &PluginStore,
    f: impl FnOnce(&PluginStore) -> Result<T> + Send + 'static,
) -> std::result::Result<T, RpcError> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|e| RpcError::from(Error::Internal(format!("plugin task failed: {e}"))))?
        .map_err(RpcError::from)
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

fn to_value<T: serde::Serialize>(value: T) -> std::result::Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::from(Error::from(e)))
}

/// Tells every connected host to re-read the plugin list whenever a plugin
/// changes (install, update, enable, disable, grants, crashes, removal).
/// Runs until the event bus is dropped.
pub type Notifier = std::sync::Arc<dyn Fn(&str, &Value) + Send + Sync>;

pub fn forward_changes(
    events: &ghostify_core::EventBus,
    notifier: Notifier,
) -> tokio::task::JoinHandle<()> {
    let mut receiver = events.subscribe();
    tokio::spawn(async move {
        loop {
            match receiver.recv().await {
                Ok(
                    ghostify_core::Event::PluginState { .. }
                    | ghostify_core::Event::PluginCrash { .. },
                ) => notifier("plugins.changed", &Value::Null),
                Ok(_) => {}
                // Missed events still mean "something changed".
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    notifier("plugins.changed", &Value::Null)
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

/// Registers `plugin.*` methods. The host calls these on behalf of sandboxed
/// plugins; every one re-checks enabled state and grants in the database, so
/// a plugin cannot do more than the user allowed even if the host is confused.
pub fn register(store: &PluginStore, router: &mut Router) {
    let s = store.clone();
    router.register("plugin.enabled", move |_, _| {
        let s = s.clone();
        async move { to_value(blocking(&s, PluginStore::enabled).await?) }
    });

    let s = store.clone();
    router.register("plugin.load", move |_, params| {
        let s = s.clone();
        async move {
            let p: IdParams = parse(params)?;
            to_value(blocking(&s, move |s| s.load(&p.id)).await?)
        }
    });

    let s = store.clone();
    router.register("plugin.authorize", move |_, params| {
        let s = s.clone();
        async move {
            #[derive(Deserialize)]
            struct P {
                id: String,
                permission: String,
                #[serde(default)]
                host: Option<String>,
            }
            let p: P = parse(params)?;
            let permission = Permission::parse(&p.permission).map_err(RpcError::from)?;
            blocking(&s, move |s| {
                s.authorize(&p.id, permission, p.host.as_deref())
            })
            .await?;
            Ok(json!(true))
        }
    });

    let s = store.clone();
    router.register("plugin.report_crash", move |_, params| {
        let s = s.clone();
        async move {
            #[derive(Deserialize)]
            struct P {
                id: String,
                kind: String,
                #[serde(default)]
                detail: String,
            }
            let p: P = parse(params)?;
            let kind = match p.kind.as_str() {
                "crash" => PluginEventKind::Crash,
                "timeout" => PluginEventKind::Timeout,
                _ => {
                    return Err(RpcError::invalid_params(
                        "kind must be `crash` or `timeout`",
                    ))
                }
            };
            to_value(blocking(&s, move |s| s.report_crash(&p.id, kind, &p.detail)).await?)
        }
    });

    let s = store.clone();
    router.register("plugin.report_state", move |_, params| {
        let s = s.clone();
        async move {
            #[derive(Deserialize)]
            struct P {
                id: String,
                state: PluginState,
            }
            let p: P = parse(params)?;
            blocking(&s, move |s| s.report_state(&p.id, p.state)).await?;
            Ok(Value::Null)
        }
    });

    let s = store.clone();
    router.register("plugin.storage.get", move |_, params| {
        let s = s.clone();
        async move {
            #[derive(Deserialize)]
            struct P {
                id: String,
                key: String,
            }
            let p: P = parse(params)?;
            Ok(blocking(&s, move |s| s.storage_get(&p.id, &p.key))
                .await?
                .unwrap_or(Value::Null))
        }
    });

    let s = store.clone();
    router.register("plugin.storage.set", move |_, params| {
        let s = s.clone();
        async move {
            #[derive(Deserialize)]
            struct P {
                id: String,
                key: String,
                value: Value,
            }
            let p: P = parse(params)?;
            blocking(&s, move |s| s.storage_set(&p.id, &p.key, &p.value)).await?;
            Ok(Value::Null)
        }
    });

    let s = store.clone();
    router.register("plugin.storage.delete", move |_, params| {
        let s = s.clone();
        async move {
            #[derive(Deserialize)]
            struct P {
                id: String,
                key: String,
            }
            let p: P = parse(params)?;
            Ok(json!(
                blocking(&s, move |s| s.storage_delete(&p.id, &p.key)).await?
            ))
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::{store, write_plugin};
    use crate::store::{InstallOptions, PluginSource};
    use ghostify_bridge::protocol::{INVALID_PARAMS, PERMISSION_DENIED};
    use ghostify_bridge::router::CallContext;

    const CTX: CallContext = CallContext { session_id: 1 };

    fn setup() -> (Router, PluginStore, tempfile::TempDir) {
        let (store, tmp) = store();
        let src = tmp.path().join("src");
        write_plugin(
            &src,
            "me/p",
            "1.0.0",
            r#","permissions":["storage","network"],"hosts":["api.example.com"]"#,
            "run()",
        );
        store
            .install_dir(&src, InstallOptions::new(PluginSource::Local))
            .unwrap();
        let mut router = Router::new();
        register(&store, &mut router);
        (router, store, tmp)
    }

    #[tokio::test]
    async fn disabled_plugins_get_nothing() {
        let (router, _store, _tmp) = setup();
        let err = router
            .dispatch(CTX, "plugin.load", json!({"id": "me/p"}))
            .await
            .unwrap_err();
        assert_eq!(err.code, PERMISSION_DENIED);
        assert_eq!(
            router
                .dispatch(CTX, "plugin.enabled", Value::Null)
                .await
                .unwrap(),
            json!([])
        );
        let err = router
            .dispatch(CTX, "plugin.storage.get", json!({"id": "me/p", "key": "k"}))
            .await
            .unwrap_err();
        assert_eq!(err.code, PERMISSION_DENIED);
    }

    #[tokio::test]
    async fn full_host_flow() {
        let (router, store, _tmp) = setup();
        store.grant("me/p", Permission::Storage).unwrap();
        store.grant("me/p", Permission::Network).unwrap();
        store.enable("me/p").unwrap();

        let list = router
            .dispatch(CTX, "plugin.enabled", Value::Null)
            .await
            .unwrap();
        assert_eq!(list[0]["id"], "me/p");
        assert_eq!(list[0]["permissions"], json!(["network", "storage"]));
        assert_eq!(list[0]["hosts"], json!(["api.example.com"]));
        assert!(list[0].get("code").is_none());
        let loaded = router
            .dispatch(CTX, "plugin.load", json!({"id": "me/p"}))
            .await
            .unwrap();
        assert_eq!(loaded["code"], "run()");

        let call = |m: &'static str, p: Value| router.dispatch(CTX, m, p);
        call(
            "plugin.storage.set",
            json!({"id": "me/p", "key": "k", "value": {"a": 1}}),
        )
        .await
        .unwrap();
        assert_eq!(
            call("plugin.storage.get", json!({"id": "me/p", "key": "k"}))
                .await
                .unwrap(),
            json!({"a": 1})
        );
        assert_eq!(
            call("plugin.storage.delete", json!({"id": "me/p", "key": "k"}))
                .await
                .unwrap(),
            json!(true)
        );

        assert_eq!(
            call(
                "plugin.authorize",
                json!({"id": "me/p", "permission": "network", "host": "api.example.com"})
            )
            .await
            .unwrap(),
            json!(true)
        );
        let err = call(
            "plugin.authorize",
            json!({"id": "me/p", "permission": "network", "host": "evil.com"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, PERMISSION_DENIED);
        let err = call(
            "plugin.authorize",
            json!({"id": "me/p", "permission": "root"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);

        call(
            "plugin.report_state",
            json!({"id": "me/p", "state": "active"}),
        )
        .await
        .unwrap();
        assert!(call(
            "plugin.report_state",
            json!({"id": "me/p", "state": "removed"})
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn crash_reports_apply_the_policy() {
        let (router, store, _tmp) = setup();
        store.enable("me/p").unwrap();
        let out = router
            .dispatch(
                CTX,
                "plugin.report_crash",
                json!({"id": "me/p", "kind": "crash", "detail": "boom"}),
            )
            .await
            .unwrap();
        assert_eq!(out["crashes_in_window"], 1);
        assert_eq!(out["disabled"], false);
        let err = router
            .dispatch(
                CTX,
                "plugin.report_crash",
                json!({"id": "me/p", "kind": "meltdown"}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        let err = router
            .dispatch(CTX, "plugin.load", json!({"nope": 1}))
            .await
            .unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn changes_are_forwarded_to_the_renderer() {
        let (store, tmp) = store();
        let bus = ghostify_core::EventBus::new(32);
        let store = store.with_events(bus.clone());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let task = forward_changes(
            &bus,
            std::sync::Arc::new(move |method, _| sink.lock().unwrap().push(method.to_owned())),
        );

        let src = tmp.path().join("src");
        write_plugin(
            &src,
            "me/p",
            "1.0.0",
            r#","permissions":["storage"]"#,
            "run()",
        );
        store
            .install_dir(&src, InstallOptions::new(PluginSource::Local))
            .unwrap();
        store.grant("me/p", Permission::Storage).unwrap();
        store.enable("me/p").unwrap();
        store.disable("me/p").unwrap();
        store.revoke("me/p", Permission::Storage).unwrap();
        store.remove("me/p").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // install, grant, enable, disable, revoke, remove
        assert_eq!(seen.lock().unwrap().len(), 6);
        assert!(seen.lock().unwrap().iter().all(|m| m == "plugins.changed"));
        drop((store, bus));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn management_methods_are_not_exposed_to_the_renderer() {
        let (router, _store, _tmp) = setup();
        for method in [
            "plugin.install",
            "plugin.enable",
            "plugin.grant",
            "plugin.remove",
            "plugin.disable",
        ] {
            assert!(!router.has_method(method), "{method}");
        }
    }
}
