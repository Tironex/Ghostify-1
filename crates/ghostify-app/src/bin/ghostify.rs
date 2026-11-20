//! Command line interface of Ghostify.

use clap::{Parser, Subcommand};
use ghostify_app::{App, Core, CoreOptions};
use ghostify_core::{logging, Error, Result};
use ghostify_db::repo::market::{ListingKind, SearchQuery};
use ghostify_db::repo::plugins::PluginSource;
use ghostify_db::repo::themes::ThemeSource;
use ghostify_plugins::store::InstallOptions;
use ghostify_plugins::Permission;
use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "ghostify",
    version,
    about = "Customise Spotify Desktop with themes and plugins"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start Ghostify: bridge to Spotify, live themes, automatic repatching, marketplace updates.
    Run,
    /// Show detected Spotify installations and their patch state.
    Status,
    /// Patch Spotify (all installations, or one with --install).
    Patch {
        #[arg(long)]
        install: Option<i64>,
    },
    /// Repair patched installations, e.g. after a Spotify update.
    Repair,
    /// Remove the patch and restore the original files.
    Restore {
        #[arg(long)]
        install: Option<i64>,
    },
    #[command(subcommand)]
    Theme(ThemeCommand),
    #[command(subcommand)]
    Plugin(PluginCommand),
    #[command(subcommand)]
    Market(MarketCommand),
}

#[derive(Subcommand)]
enum ThemeCommand {
    List,
    /// Install from a folder or a .zip file (Spicetify themes work too).
    Install {
        path: PathBuf,
        #[arg(long)]
        replace: bool,
    },
    Activate {
        id: String,
        #[arg(long)]
        scheme: Option<String>,
    },
    Deactivate,
    Remove {
        id: String,
    },
    Export {
        id: String,
        out: PathBuf,
    },
    /// `dark`, `light` or `system`.
    Mode {
        mode: Option<String>,
    },
}

#[derive(Subcommand)]
enum PluginCommand {
    List,
    /// Install from a folder or a .zip file. The scanner runs first.
    Install {
        path: PathBuf,
        #[arg(long)]
        replace: bool,
        /// Install although the scanner blocked it.
        #[arg(long)]
        allow_blocked: bool,
    },
    Info {
        id: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Grant {
        id: String,
        permission: String,
    },
    Revoke {
        id: String,
        permission: String,
    },
    Remove {
        id: String,
    },
}

#[derive(Subcommand)]
enum MarketCommand {
    /// Fetch the signed index now.
    Refresh,
    Search {
        text: Vec<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        category: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Categories,
    Show {
        id: String,
    },
    Install {
        id: String,
        #[arg(long)]
        allow_blocked: bool,
    },
    /// List installed items with a newer version.
    Updates,
    Update {
        id: String,
        /// Accept permissions the new version asks for in addition.
        #[arg(long)]
        allow_new_permissions: bool,
    },
    /// Apply updates the configured policy allows.
    AutoUpdate,
    /// Rate a listing from 1 to 5 (stored on this computer only).
    Rate {
        id: String,
        score: u8,
    },
}

fn print<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Error::Internal(format!("task failed: {e}")))?
}

fn parse_kind(text: &str) -> Result<ListingKind> {
    ListingKind::parse(text)
        .map_err(|_| Error::Invalid("--kind must be `plugin` or `theme`".into()))
}

async fn run(cli: Cli) -> Result<()> {
    let core = Core::open(CoreOptions::discover()?)?;
    match cli.command {
        Command::Run => {
            let app = App::start(core).await?;
            eprintln!(
                "Ghostify is running (bridge on 127.0.0.1:{}). Press Ctrl-C to stop.",
                app.bridge_port()
            );
            tokio::signal::ctrl_c().await?;
            app.shutdown().await;
        }
        Command::Status => {
            let installs = core.patch.detect_and_sync().await?;
            let mut reports = Vec::new();
            for install in installs {
                reports.push(core.patch.status(install.id).await?);
            }
            print(&reports)?;
        }
        Command::Patch { install } => {
            let ids = install_ids(&core, install).await?;
            let mut reports = Vec::new();
            for id in ids {
                reports.push(core.patch.patch(id).await?);
            }
            print(&reports)?;
        }
        Command::Repair => {
            core.patch.detect_and_sync().await?;
            let mut reports = Vec::new();
            for (id, result) in core.patch.repair_all().await {
                reports.push(match result {
                    Ok(report) => serde_json::to_value(report)?,
                    Err(err) => serde_json::json!({ "install_id": id, "error": err.to_string() }),
                });
            }
            print(&reports)?;
        }
        Command::Restore { install } => {
            let ids = install_ids(&core, install).await?;
            let mut reports = Vec::new();
            for id in ids {
                reports.push(core.patch.restore(id).await?);
            }
            print(&reports)?;
        }
        Command::Theme(command) => theme(core, command).await?,
        Command::Plugin(command) => plugin(core, command).await?,
        Command::Market(command) => market(core, command).await?,
    }
    Ok(())
}

async fn install_ids(core: &Core, only: Option<i64>) -> Result<Vec<i64>> {
    let installs = core.patch.detect_and_sync().await?;
    match only {
        Some(id) if installs.iter().any(|i| i.id == id) => Ok(vec![id]),
        Some(id) => Err(Error::NotFound(format!("installation {id}"))),
        None if installs.is_empty() => Err(Error::NotFound("no Spotify installation found".into())),
        None => Ok(installs.into_iter().map(|i| i.id).collect()),
    }
}

async fn theme(core: Core, command: ThemeCommand) -> Result<()> {
    let store = core.themes;
    match command {
        ThemeCommand::List => print(&blocking(move || store.list()).await?)?,
        ThemeCommand::Install { path, replace } => {
            let installed = blocking(move || {
                if path.is_dir() {
                    store.install_dir(&path, ThemeSource::Local, replace)
                } else {
                    store.install_zip(&path, ThemeSource::Import, replace)
                }
            })
            .await?;
            for note in &installed.notes {
                eprintln!("note: {note}");
            }
            print(&installed.info)?;
        }
        ThemeCommand::Activate { id, scheme } => {
            print(&blocking(move || store.activate(&id, scheme.as_deref())).await?)?
        }
        ThemeCommand::Deactivate => blocking(move || store.deactivate()).await?,
        ThemeCommand::Remove { id } => blocking(move || store.remove(&id)).await?,
        ThemeCommand::Export { id, out } => blocking(move || store.export(&id, &out)).await?,
        ThemeCommand::Mode { mode } => {
            let result = blocking(move || {
                if let Some(mode) = mode {
                    store.set_mode(&mode)?;
                }
                store.mode()
            })
            .await?;
            println!("{result}");
        }
    }
    Ok(())
}

async fn plugin(core: Core, command: PluginCommand) -> Result<()> {
    let store = core.plugins;
    match command {
        PluginCommand::List => print(&blocking(move || store.list()).await?)?,
        PluginCommand::Install {
            path,
            replace,
            allow_blocked,
        } => {
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
            for note in &installed.notes {
                eprintln!("note: {note}");
            }
            for finding in &installed.report.findings {
                eprintln!("scan: {finding:?}");
            }
            eprintln!("scan verdict: {:?}", installed.report.verdict);
            print(&installed.info)?;
        }
        PluginCommand::Info { id } => print(&blocking(move || store.info(&id)).await?)?,
        PluginCommand::Enable { id } => print(&blocking(move || store.enable(&id)).await?)?,
        PluginCommand::Disable { id } => print(&blocking(move || store.disable(&id)).await?)?,
        PluginCommand::Grant { id, permission } => {
            blocking(move || store.grant(&id, Permission::parse(&permission)?)).await?
        }
        PluginCommand::Revoke { id, permission } => {
            let had = blocking(move || store.revoke(&id, Permission::parse(&permission)?)).await?;
            if !had {
                eprintln!("that permission was not granted");
            }
        }
        PluginCommand::Remove { id } => blocking(move || store.remove(&id)).await?,
    }
    Ok(())
}

async fn market(core: Core, command: MarketCommand) -> Result<()> {
    let market = core.market;
    match command {
        MarketCommand::Refresh => print(&blocking(move || market.refresh(true)).await?)?,
        MarketCommand::Search {
            text,
            kind,
            category,
            limit,
        } => {
            let query = SearchQuery {
                text: text.join(" "),
                kind: kind.as_deref().map(parse_kind).transpose()?,
                category,
                limit,
                offset: 0,
            };
            print(&blocking(move || market.search(&query)).await?)?
        }
        MarketCommand::Categories => print(&blocking(move || market.categories()).await?)?,
        MarketCommand::Show { id } => print(&blocking(move || market.get(&id)).await?)?,
        MarketCommand::Install { id, allow_blocked } => {
            let shown = id.clone();
            blocking(move || market.install(&id, allow_blocked)).await?;
            println!("installed {shown}");
        }
        MarketCommand::Updates => print(&blocking(move || market.updates()).await?)?,
        MarketCommand::Update {
            id,
            allow_new_permissions,
        } => print(&blocking(move || market.update(&id, allow_new_permissions)).await?)?,
        MarketCommand::AutoUpdate => print(&blocking(move || market.auto_update()).await?)?,
        MarketCommand::Rate { id, score } => {
            print(&blocking(move || market.rate(&id, score)).await?)?
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = logging::init("warn,ghostify=info");
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
