//! The calls the desktop interface makes. One entry point, `Api::call`, with
//! named methods and JSON parameters, so the Tauri shell needs a single command
//! and the same surface can be tested without a window.

use crate::App;
use ghostify_core::{Config, Error, Result};
use ghostify_db::repo::market::SearchQuery;
use ghostify_db::repo::plugins::PluginSource;
use ghostify_db::repo::themes::ThemeSource;
use ghostify_extras::{HotkeyAction, PlayerCommand};
use ghostify_plugins::store::InstallOptions;
use ghostify_plugins::Permission;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct Api {
    app: Arc<App>,
}

#[derive(Deserialize)]
struct Id {
    id: String,
}

#[derive(Deserialize)]
struct InstallRef {
    #[serde(default)]
    install: Option<i64>,
}

#[derive(Deserialize)]
struct PathParams {
    path: PathBuf,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    allow_blocked: bool,
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T> {
    // A missing parameter object means "no parameters".
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value).map_err(|e| Error::Invalid(format!("invalid parameters: {e}")))
}

fn value<T: serde::Serialize>(data: T) -> Result<Value> {
    Ok(serde_json::to_value(data)?)
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::Internal(format!("task failed: {e}")))?
}

impl Api {
    pub fn new(app: Arc<App>) -> Api {
        Api { app }
    }

    pub async fn call(&self, method: &str, input: Value) -> Result<Value> {
        let core = &self.app.core;
        match method {
            "app.info" => value(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "bridge_port": self.app.bridge_port(),
                "connected_loaders": self.app.connected_loaders(),
                "data_dir": core.paths.data_dir(),
                "lyrics_dir": core.lyrics.dir(),
            })),

            // ---- Spotify patch ------------------------------------------
            "patch.status" => {
                let mut reports = Vec::new();
                for install in core.patch.detect_and_sync().await? {
                    reports.push(core.patch.status(install.id).await?);
                }
                value(reports)
            }
            "patch.apply" | "patch.restore" => {
                let InstallRef { install } = params(input)?;
                let ids = self.install_ids(install).await?;
                let mut reports = Vec::new();
                for id in ids {
                    reports.push(if method == "patch.apply" {
                        core.patch.patch(id).await?
                    } else {
                        core.patch.restore(id).await?
                    });
                }
                value(reports)
            }
            "patch.repair" => {
                core.patch.detect_and_sync().await?;
                let results: Vec<Value> = core
                    .patch
                    .repair_all()
                    .await
                    .into_iter()
                    .map(|(id, result)| match result {
                        Ok(report) => json!({ "install_id": id, "report": report }),
                        Err(err) => json!({ "install_id": id, "error": err.to_string() }),
                    })
                    .collect();
                value(results)
            }

            // ---- themes -------------------------------------------------
            "theme.list" => value(self.app.themes.list().await?),
            "theme.current" => value(self.app.themes.current().await?),
            "theme.mode" => {
                let store = core.themes.clone();
                value(blocking(move || store.mode()).await?)
            }
            "theme.set_mode" => {
                #[derive(Deserialize)]
                struct Mode {
                    mode: String,
                }
                let Mode { mode } = params(input)?;
                self.app.themes.set_mode(&mode).await?;
                Ok(Value::Null)
            }
            "theme.activate" => {
                #[derive(Deserialize)]
                struct Activate {
                    id: String,
                    #[serde(default)]
                    scheme: Option<String>,
                }
                let Activate { id, scheme } = params(input)?;
                value(self.app.themes.activate(&id, scheme.as_deref()).await?)
            }
            "theme.deactivate" => {
                self.app.themes.deactivate().await?;
                Ok(Value::Null)
            }
            "theme.remove" => {
                let Id { id } = params(input)?;
                self.app.themes.remove(&id).await?;
                Ok(Value::Null)
            }
            "theme.export" => {
                #[derive(Deserialize)]
                struct Export {
                    id: String,
                    out: PathBuf,
                }
                let Export { id, out } = params(input)?;
                self.app.themes.export(&id, out).await?;
                Ok(Value::Null)
            }
            "theme.install" => {
                let PathParams { path, replace, .. } = params(input)?;
                let installed = if path.is_dir() {
                    self.app
                        .themes
                        .install_dir(path, ThemeSource::Local, replace)
                        .await?
                } else {
                    self.app
                        .themes
                        .install_zip(path, ThemeSource::Import, replace)
                        .await?
                };
                value(json!({ "info": installed.info, "notes": installed.notes }))
            }

            // ---- plugins ------------------------------------------------
            "plugin.list" => {
                let store = core.plugins.clone();
                value(blocking(move || store.list()).await?)
            }
            "plugin.info" => {
                let Id { id } = params(input)?;
                let store = core.plugins.clone();
                value(blocking(move || store.info(&id)).await?)
            }
            "plugin.install" => {
                let PathParams {
                    path,
                    replace,
                    allow_blocked,
                } = params(input)?;
                let store = core.plugins.clone();
                let installed = blocking(move || {
                    let mut options = InstallOptions::new(PluginSource::Local);
                    options.replace = replace;
                    options.allow_blocked = allow_blocked;
                    if path.is_dir() {
                        store.install_dir(&path, options)
                    } else {
                        store.install_zip(&path, options)
                    }
                })
                .await?;
                value(
                    json!({ "info": installed.info, "report": installed.report, "notes": installed.notes }),
                )
            }
            "plugin.enable" | "plugin.disable" => {
                let Id { id } = params(input)?;
                let store = core.plugins.clone();
                let enable = method == "plugin.enable";
                value(
                    blocking(move || {
                        if enable {
                            store.enable(&id)
                        } else {
                            store.disable(&id)
                        }
                    })
                    .await?,
                )
            }
            "plugin.grant" | "plugin.revoke" => {
                #[derive(Deserialize)]
                struct Grant {
                    id: String,
                    permission: String,
                }
                let Grant { id, permission } = params(input)?;
                let store = core.plugins.clone();
                let grant = method == "plugin.grant";
                blocking(move || {
                    let permission = Permission::parse(&permission)?;
                    if grant {
                        store.grant(&id, permission)?;
                    } else {
                        store.revoke(&id, permission)?;
                    }
                    store.info(&id)
                })
                .await
                .and_then(value)
            }
            "plugin.remove" => {
                let Id { id } = params(input)?;
                let store = core.plugins.clone();
                blocking(move || store.remove(&id)).await?;
                Ok(Value::Null)
            }

            // ---- marketplace --------------------------------------------
            "market.refresh" => {
                let market = core.market.clone();
                value(blocking(move || market.refresh(true)).await?)
            }
            "market.search" => {
                let query: SearchQuery = params(input)?;
                let market = core.market.clone();
                value(blocking(move || market.search(&query)).await?)
            }
            "market.categories" => {
                let market = core.market.clone();
                value(blocking(move || market.categories()).await?)
            }
            "market.get" => {
                let Id { id } = params(input)?;
                let market = core.market.clone();
                value(blocking(move || market.get(&id)).await?)
            }
            "market.package" => {
                let Id { id } = params(input)?;
                let market = core.market.clone();
                value(blocking(move || market.package_info(&id)).await?)
            }
            "market.install" => {
                #[derive(Deserialize)]
                struct Install {
                    id: String,
                    #[serde(default)]
                    allow_blocked: bool,
                }
                let Install { id, allow_blocked } = params(input)?;
                let market = core.market.clone();
                blocking(move || market.install(&id, allow_blocked)).await?;
                Ok(Value::Null)
            }
            "market.updates" => {
                let market = core.market.clone();
                value(blocking(move || market.updates()).await?)
            }
            "market.update" => {
                #[derive(Deserialize)]
                struct Update {
                    id: String,
                    #[serde(default)]
                    allow_new_permissions: bool,
                }
                let Update {
                    id,
                    allow_new_permissions,
                } = params(input)?;
                let market = core.market.clone();
                value(blocking(move || market.update(&id, allow_new_permissions)).await?)
            }
            "market.rate" => {
                #[derive(Deserialize)]
                struct Rate {
                    id: String,
                    score: u8,
                }
                let Rate { id, score } = params(input)?;
                let market = core.market.clone();
                value(blocking(move || market.rate(&id, score)).await?)
            }
            "market.my_rating" => {
                let Id { id } = params(input)?;
                let market = core.market.clone();
                value(blocking(move || market.my_rating(&id)).await?)
            }

            // ---- player, lyrics, hotkeys -------------------------------
            "player.state" => value(core.player.snapshot()),
            "player.command" => {
                #[derive(Deserialize)]
                struct Command {
                    action: PlayerCommand,
                }
                let Command { action } = params(input)?;
                core.player.command(action)?;
                Ok(Value::Null)
            }
            "lyrics.current" => {
                let track = core.player.snapshot().track;
                let lyrics = match track.clone() {
                    Some(track) => {
                        let service = core.lyrics.clone();
                        blocking(move || service.get(&track)).await?
                    }
                    None => None,
                };
                value(json!({ "track": track, "lyrics": lyrics }))
            }
            "hotkeys.list" => {
                let service = core.hotkeys.clone();
                value(blocking(move || service.list()).await?)
            }
            "hotkeys.set" => {
                #[derive(Deserialize)]
                struct Set {
                    action: String,
                    accelerator: String,
                    #[serde(default = "enabled")]
                    enabled: bool,
                }
                fn enabled() -> bool {
                    true
                }
                let Set {
                    action,
                    accelerator,
                    enabled,
                } = params(input)?;
                let service = core.hotkeys.clone();
                value(blocking(move || service.set(&action, &accelerator, enabled)).await?)
            }
            "hotkeys.reset" => {
                #[derive(Deserialize)]
                struct Reset {
                    action: String,
                }
                let Reset { action } = params(input)?;
                let service = core.hotkeys.clone();
                value(blocking(move || service.reset(&action)).await?)
            }
            // Runs the action of a hotkey. Actions that belong to the window
            // (the mini player) are returned for the shell to carry out.
            "hotkeys.trigger" => {
                #[derive(Deserialize)]
                struct Trigger {
                    action: String,
                }
                let Trigger { action } = params(input)?;
                match HotkeyAction::parse(&action)? {
                    HotkeyAction::PlayPause => core.player.command(PlayerCommand::Toggle)?,
                    HotkeyAction::Next => core.player.command(PlayerCommand::Next)?,
                    HotkeyAction::Previous => core.player.command(PlayerCommand::Previous)?,
                    HotkeyAction::ToggleMiniPlayer => {
                        return Ok(json!({ "ui": "toggle_mini_player" }))
                    }
                }
                Ok(Value::Null)
            }

            // ---- settings -----------------------------------------------
            "config.get" => value(core.config.as_ref()),
            "config.save" => {
                #[derive(Deserialize)]
                struct Save {
                    config: Config,
                }
                let Save { config } = params(input)?;
                let file = core.paths.config_file();
                blocking(move || config.save(&file)).await?;
                // Most settings are read at start-up.
                Ok(json!({ "restart_required": true }))
            }
            other => Err(Error::NotFound(format!("unknown method `{other}`"))),
        }
    }

    async fn install_ids(&self, only: Option<i64>) -> Result<Vec<i64>> {
        let installs = self.app.core.patch.detect_and_sync().await?;
        match only {
            Some(id) if installs.iter().any(|i| i.id == id) => Ok(vec![id]),
            Some(id) => Err(Error::NotFound(format!("installation {id}"))),
            None if installs.is_empty() => {
                Err(Error::NotFound("no Spotify installation found".into()))
            }
            None => Ok(installs.into_iter().map(|i| i.id).collect()),
        }
    }
}
