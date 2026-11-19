// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ghostify_app::api::Api;
use ghostify_app::{App, Core, CoreOptions};
use ghostify_core::{ApiError, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

const MINI_PLAYER: &str = "mini";

#[derive(Deserialize)]
struct Hotkey {
    action: String,
    accelerator: String,
    enabled: bool,
}

/// The interface's single entry point. Methods and parameters are documented in
/// `crates/ghostify-app/src/api.rs`.
#[tauri::command]
async fn ghostify_call(
    app: AppHandle,
    api: tauri::State<'_, Api>,
    method: String,
    params: Value,
) -> Result<Value, ApiError> {
    let result = api.call(&method, params).await.map_err(ApiError::from)?;
    // Shortcuts are registered with the system, so a change has to be applied here.
    if matches!(method.as_str(), "hotkeys.set" | "hotkeys.reset") {
        register_hotkeys(&app, &api).await;
    }
    Ok(result)
}

/// Shows the mini player window, creating it on first use, or hides it.
fn toggle_mini(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(MINI_PLAYER) {
        if window.is_visible()? {
            return window.hide();
        }
        window.show()?;
        return window.set_focus();
    }
    WebviewWindowBuilder::new(app, MINI_PLAYER, WebviewUrl::App("index.html#/mini".into()))
        .title("Ghostify – Mini player")
        .inner_size(360.0, 560.0)
        .min_inner_size(300.0, 420.0)
        .always_on_top(true)
        .build()
        .map(|_| ())
}

#[tauri::command]
async fn toggle_mini_player(app: AppHandle) -> Result<(), ApiError> {
    toggle_mini(&app).map_err(|e| ApiError::from(Error::Internal(e.to_string())))
}

/// Replaces all registered global shortcuts with the stored, enabled ones. A
/// shortcut another program already holds is skipped, not fatal.
async fn register_hotkeys(app: &AppHandle, api: &Api) {
    let shortcuts = app.global_shortcut();
    if let Err(err) = shortcuts.unregister_all() {
        tracing::warn!(%err, "could not clear the global shortcuts");
    }
    let list: Vec<Hotkey> = match api.call("hotkeys.list", Value::Null).await {
        Ok(value) => serde_json::from_value(value).unwrap_or_default(),
        Err(err) => {
            tracing::warn!(%err, "could not read the hotkeys");
            return;
        }
    };
    for hotkey in list.into_iter().filter(|h| h.enabled) {
        let (handle, api, action) = (app.clone(), api.clone(), hotkey.action.clone());
        let registered = shortcuts.on_shortcut(hotkey.accelerator.as_str(), move |_, _, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            let (handle, api, action) = (handle.clone(), api.clone(), action.clone());
            tauri::async_runtime::spawn(async move {
                match api.call("hotkeys.trigger", json!({ "action": action })).await {
                    Ok(answer) if answer["ui"] == "toggle_mini_player" => {
                        if let Err(err) = toggle_mini(&handle) {
                            tracing::warn!(%err, "could not toggle the mini player");
                        }
                    }
                    Ok(_) => {}
                    // Typically "Spotify is not connected"; nothing to show for a key press.
                    Err(err) => tracing::debug!(%err, action, "hotkey action failed"),
                }
            });
        });
        if let Err(err) = registered {
            tracing::warn!(%err, accelerator = hotkey.accelerator, "could not register the shortcut");
        }
    }
}

fn main() {
    let _ = ghostify_core::logging::init("warn,ghostify=info");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // The services must run before the first page asks for them.
            let api = tauri::async_runtime::block_on(async {
                let core = Core::open(CoreOptions::discover()?)?;
                let running = App::start(core).await?;
                Ok::<_, Error>(Api::new(Arc::new(running)))
            })?;
            app.manage(api.clone());
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move { register_hotkeys(&handle, &api).await });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![ghostify_call, toggle_mini_player])
        .run(tauri::generate_context!())
        .expect("error while running Ghostify");
}
