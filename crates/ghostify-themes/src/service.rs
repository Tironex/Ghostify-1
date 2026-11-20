//! Ties the store to the running app: live reload, push to the loader, bridge methods.

use crate::store::{Current, Installed, ThemeInfo, ThemeStore};
use crate::watch::ThemeWatcher;
use ghostify_bridge::{Router, RpcError};
use ghostify_core::{Error, Result};
use ghostify_db::repo::themes::ThemeSource;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;

/// Sends a notification to every connected loader (usually `Bridge::notify`).
pub type Notifier = Arc<dyn Fn(&str, &Value) + Send + Sync>;

struct Live {
    _watcher: ThemeWatcher,
    task: JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct ThemeService {
    store: ThemeStore,
    notifier: Notifier,
    debounce: Duration,
    live: Mutex<Option<Live>>,
}

impl ThemeService {
    pub fn new(store: ThemeStore, notifier: Notifier, debounce: Duration) -> Arc<ThemeService> {
        Arc::new(ThemeService {
            store,
            notifier,
            debounce,
            live: Mutex::new(None),
        })
    }

    pub fn store(&self) -> &ThemeStore {
        &self.store
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&ThemeStore) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|e| Error::Internal(format!("theme task failed: {e}")))?
    }

    /// Starts watching the active theme and pushes its CSS to connected loaders.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.restart_watch().await?;
        self.push().await
    }

    pub async fn list(&self) -> Result<Vec<ThemeInfo>> {
        self.blocking(ThemeStore::list).await
    }

    pub async fn current(&self) -> Result<Current> {
        self.blocking(ThemeStore::current).await
    }

    pub async fn activate(self: &Arc<Self>, id: &str, scheme: Option<&str>) -> Result<ThemeInfo> {
        let (id, scheme) = (id.to_owned(), scheme.map(str::to_owned));
        let info = self
            .blocking(move |s| s.activate(&id, scheme.as_deref()))
            .await?;
        self.restart_watch().await?;
        self.push().await?;
        Ok(info)
    }

    pub async fn deactivate(self: &Arc<Self>) -> Result<()> {
        self.blocking(|s| s.deactivate()).await?;
        self.restart_watch().await?;
        self.push().await
    }

    pub async fn set_mode(&self, mode: &str) -> Result<()> {
        let mode = mode.to_owned();
        self.blocking(move |s| s.set_mode(&mode)).await?;
        self.push().await
    }

    pub async fn install_dir(
        self: &Arc<Self>,
        src: PathBuf,
        source: ThemeSource,
        replace: bool,
    ) -> Result<Installed> {
        let installed = self
            .blocking(move |s| s.install_dir(&src, source, replace))
            .await?;
        self.after_install(&installed).await?;
        Ok(installed)
    }

    pub async fn install_zip(
        self: &Arc<Self>,
        zip: PathBuf,
        source: ThemeSource,
        replace: bool,
    ) -> Result<Installed> {
        let installed = self
            .blocking(move |s| s.install_zip(&zip, source, replace))
            .await?;
        self.after_install(&installed).await?;
        Ok(installed)
    }

    async fn after_install(self: &Arc<Self>, installed: &Installed) -> Result<()> {
        if installed.info.theme.active {
            self.restart_watch().await?;
            self.push().await?;
        }
        Ok(())
    }

    pub async fn remove(self: &Arc<Self>, id: &str) -> Result<()> {
        let id = id.to_owned();
        let was_active = self
            .blocking({
                let id = id.clone();
                move |s| Ok(s.active()?.is_some_and(|t| t.id == id))
            })
            .await?;
        self.blocking(move |s| s.remove(&id)).await?;
        if was_active {
            self.restart_watch().await?;
            self.push().await?;
        }
        Ok(())
    }

    pub async fn export(&self, id: &str, out: PathBuf) -> Result<()> {
        let id = id.to_owned();
        self.blocking(move |s| s.export(&id, &out)).await
    }

    /// Sends the current CSS and scheme to all loaders.
    pub async fn push(&self) -> Result<()> {
        let current = self.current().await?;
        (self.notifier)(
            "theme.css",
            &json!({ "css": current.css, "theme_id": current.theme_id, "warnings": current.warnings }),
        );
        (self.notifier)("theme.scheme", &json!({ "scheme": current.scheme }));
        Ok(())
    }

    async fn restart_watch(self: &Arc<Self>) -> Result<()> {
        let dir = self
            .blocking(|s| s.active()?.map(|t| s.theme_dir(&t.id)).transpose())
            .await?;
        let live = match dir {
            None => None,
            Some(dir) => {
                let (watcher, mut signals) = ThemeWatcher::start(&dir, self.debounce)?;
                let weak: Weak<ThemeService> = Arc::downgrade(self);
                let task = tokio::spawn(async move {
                    while signals.recv().await.is_some() {
                        let Some(service) = weak.upgrade() else {
                            return;
                        };
                        if let Err(err) = service.push().await {
                            tracing::warn!(%err, "theme live reload failed");
                        }
                    }
                });
                Some(Live {
                    _watcher: watcher,
                    task,
                })
            }
        };
        *self.live.lock().unwrap_or_else(|p| p.into_inner()) = live;
        Ok(())
    }

    /// Read-only methods for the loader. Changing themes stays with the app UI,
    /// so code running inside Spotify cannot switch or delete themes.
    pub fn register(self: &Arc<Self>, router: &mut Router) {
        let service = self.clone();
        router.register("theme.current", move |_, _| {
            let service = service.clone();
            async move {
                let current = service.current().await.map_err(RpcError::from)?;
                serde_json::to_value(current).map_err(|e| RpcError::from(Error::from(e)))
            }
        });
        let service = self.clone();
        router.register("theme.list", move |_, _| {
            let service = service.clone();
            async move {
                let list = service.list().await.map_err(RpcError::from)?;
                serde_json::to_value(list).map_err(|e| RpcError::from(Error::from(e)))
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testutil::{store, write_theme};
    use ghostify_bridge::router::CallContext;
    use std::sync::Mutex as StdMutex;
    use tokio::time::{sleep, Instant};

    type Log = Arc<StdMutex<Vec<(String, Value)>>>;

    fn service() -> (Arc<ThemeService>, Log, tempfile::TempDir) {
        let (store, tmp) = store();
        let log: Log = Arc::default();
        let sink = log.clone();
        let notifier: Notifier = Arc::new(move |method, params| {
            sink.lock()
                .unwrap()
                .push((method.to_owned(), params.clone()))
        });
        (
            ThemeService::new(store, notifier, Duration::from_millis(80)),
            log,
            tmp,
        )
    }

    fn css_pushes(log: &Log) -> Vec<String> {
        log.lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == "theme.css")
            .map(|(_, p)| p["css"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn wait_for(log: &Log, expected: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if css_pushes(log).last().is_some_and(|c| c == expected) {
                return true;
            }
            sleep(Duration::from_millis(50)).await;
        }
        false
    }

    #[tokio::test]
    async fn activation_pushes_css_and_scheme() {
        let (svc, log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "a{x:1}", None);
        svc.install_dir(src, ThemeSource::Local, false)
            .await
            .unwrap();
        assert!(
            log.lock().unwrap().is_empty(),
            "installing alone changes nothing visible"
        );

        svc.activate("a", None).await.unwrap();
        assert_eq!(css_pushes(&log), ["a{x:1}"]);
        svc.set_mode("light").await.unwrap();
        let last = log.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.0, "theme.scheme");
        assert_eq!(last.1["scheme"], "light");

        svc.deactivate().await.unwrap();
        assert_eq!(css_pushes(&log).last().unwrap(), "");
        assert!(svc.set_mode("neon").await.is_err());
    }

    #[tokio::test]
    async fn editing_the_active_theme_reloads_it_live() {
        let (svc, log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "v1{}", None);
        svc.install_dir(src, ThemeSource::Local, false)
            .await
            .unwrap();
        svc.activate("a", None).await.unwrap();

        let file = svc.store().theme_dir("a").unwrap().join("user.css");
        std::fs::write(&file, "v2{}").unwrap();
        assert!(wait_for(&log, "v2{}").await, "{:?}", css_pushes(&log));
        std::fs::write(&file, "v3{}").unwrap();
        assert!(wait_for(&log, "v3{}").await);

        // Broken edits do not break the page: the loader gets empty CSS and a warning.
        std::fs::write(&file, "a{width:expression(1)}").unwrap();
        assert!(wait_for(&log, "").await);
        let warnings = log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, _)| m == "theme.css")
            .unwrap()
            .1["warnings"]
            .clone();
        assert!(warnings[0].as_str().unwrap().contains("cannot be loaded"));
    }

    #[tokio::test]
    async fn switching_themes_moves_the_watch() {
        let (svc, log, tmp) = service();
        for id in ["a", "b"] {
            let src = tmp.path().join(id);
            write_theme(&src, id, &format!("{id}1{{}}"), None);
            svc.install_dir(src, ThemeSource::Local, false)
                .await
                .unwrap();
        }
        svc.activate("a", None).await.unwrap();
        svc.activate("b", None).await.unwrap();
        let before = css_pushes(&log).len();
        std::fs::write(
            svc.store().theme_dir("a").unwrap().join("user.css"),
            "stale{}",
        )
        .unwrap();
        sleep(Duration::from_millis(400)).await;
        assert_eq!(
            css_pushes(&log).len(),
            before,
            "the old theme is no longer watched"
        );
        std::fs::write(svc.store().theme_dir("b").unwrap().join("user.css"), "b2{}").unwrap();
        assert!(wait_for(&log, "b2{}").await);
    }

    #[tokio::test]
    async fn removing_the_active_theme_clears_the_page() {
        let (svc, log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", None);
        svc.install_dir(src, ThemeSource::Local, false)
            .await
            .unwrap();
        svc.activate("a", None).await.unwrap();
        svc.remove("a").await.unwrap();
        assert_eq!(css_pushes(&log).last().unwrap(), "");
        assert!(svc.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn updating_the_active_theme_pushes_new_css() {
        let (svc, log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "old{}", None);
        svc.install_dir(src.clone(), ThemeSource::Local, false)
            .await
            .unwrap();
        svc.activate("a", None).await.unwrap();
        write_theme(&src, "a", "new{}", None);
        svc.install_dir(src, ThemeSource::Local, true)
            .await
            .unwrap();
        assert_eq!(css_pushes(&log).last().unwrap(), "new{}");
    }

    #[tokio::test]
    async fn start_pushes_the_active_theme() {
        let (svc, log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", None);
        svc.install_dir(src, ThemeSource::Local, false)
            .await
            .unwrap();
        svc.store().activate("a", None).unwrap();
        svc.start().await.unwrap();
        assert_eq!(css_pushes(&log), ["x{}"]);
    }

    #[tokio::test]
    async fn bridge_methods_are_read_only_and_serve_the_loader() {
        let (svc, _log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", None);
        svc.install_dir(src, ThemeSource::Local, false)
            .await
            .unwrap();
        svc.activate("a", None).await.unwrap();

        let mut router = Router::new();
        svc.register(&mut router);
        let ctx = CallContext { session_id: 1 };
        let current = router
            .dispatch(ctx, "theme.current", Value::Null)
            .await
            .unwrap();
        assert_eq!(current["css"], "x{}");
        assert_eq!(current["scheme"], "dark");
        assert_eq!(current["theme_id"], "a");
        let list = router
            .dispatch(ctx, "theme.list", Value::Null)
            .await
            .unwrap();
        assert_eq!(list[0]["id"], "a");
        assert!(!router.has_method("theme.activate") && !router.has_method("theme.remove"));
    }

    #[tokio::test]
    async fn export_goes_through_the_service() {
        let (svc, _log, tmp) = service();
        let src = tmp.path().join("src");
        write_theme(&src, "a", "x{}", None);
        svc.install_dir(src, ThemeSource::Local, false)
            .await
            .unwrap();
        let out = tmp.path().join("a.zip");
        svc.export("a", out.clone()).await.unwrap();
        assert!(out.is_file());
        let again = svc
            .install_zip(out, ThemeSource::Import, true)
            .await
            .unwrap();
        assert_eq!(again.info.theme.source, ThemeSource::Import);
    }
}
