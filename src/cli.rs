use crate::config::AppConfig;
use crate::service::{self, ServiceStatus};
use crate::state::AppState;
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

const CATALOG_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Debug, Parser)]
#[command(
    name = "loom-router",
    version,
    about = "Headless multi-provider gateway for Codex"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the proxy in the foreground.
    Serve,
    /// Print machine-readable service and proxy state as JSON.
    Status,
    /// Manage the per-user service (launchd on macOS, systemd on Linux).
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Print the local token for Codex's provider-auth hook.
    ProviderAuth,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Install the managed binary and start it at login.
    Install {
        /// Optionally create a command symlink at this path.
        #[arg(long)]
        link: Option<PathBuf>,
    },
    /// Stop and remove the service. Configuration and credentials stay.
    Uninstall,
    /// Restart the service.
    Restart,
    /// Print the same JSON as `loom-router status`.
    Status,
}

#[derive(Debug, Serialize)]
struct StatusReport {
    version: &'static str,
    ok: bool,
    service: ServiceStatus,
    proxy: ProxyStatus,
    config: ConfigStatus,
    codex: CodexStatus,
}

#[derive(Debug, Serialize)]
struct ProxyStatus {
    running: bool,
    authenticated: bool,
    port: u16,
    url: String,
    model_count: usize,
    models: Vec<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConfigStatus {
    path: PathBuf,
    schema_version: u32,
    provider_proxies: std::collections::BTreeMap<String, String>,
    provider_count: usize,
    enabled_provider_count: usize,
    enabled_model_count: usize,
    enabled_models: Vec<String>,
    codex_integration: bool,
    native_slug_mode: bool,
    active_model: Option<String>,
    side_call_fallback: Option<String>,
}

#[derive(Debug, Serialize)]
struct CodexStatus {
    home: PathBuf,
    config_path: PathBuf,
    config_exists: bool,
    config_parseable: bool,
    managed_block_present: bool,
    managed_block_orphaned: bool,
    integration_enabled: bool,
    native_catalog_present: bool,
    merged_catalog_present: bool,
    merged_model_count: usize,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Serve) => serve(),
        Some(Command::Status) => print_status(),
        Some(Command::Service { command }) => run_service_command(command),
        Some(Command::ProviderAuth) => {
            crate::codex::print_provider_auth_token();
            Ok(())
        }
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn serve() -> Result<()> {
    init_tracing();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build the headless runtime")?;
    runtime.block_on(serve_until_shutdown())
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "loom_router=info".into()),
        )
        .try_init();
}

async fn serve_until_shutdown() -> Result<()> {
    let state = Arc::new(AppState::load());

    if let Err(error) = state.persist_migration().await {
        tracing::warn!("persisting the migrated config failed: {error}");
    }
    match tokio::task::spawn_blocking(crate::codex::ensure_codex_cli).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!("ensuring the Codex CLI failed at startup: {error}"),
        Err(error) => tracing::warn!("ensuring the Codex CLI task failed at startup: {error}"),
    }
    if let Err(error) = crate::codex::sync_orchestrator_skill() {
        tracing::warn!("orchestrator skill sync failed at startup: {error}");
    }
    state.repair_codex_integration().await;

    let refresh_state = Arc::clone(&state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(CATALOG_REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            interval.tick().await;
            refresh_state.refresh_all_model_catalogs().await;
        }
    });

    state.server_start().await?;
    tracing::info!("headless service ready");
    shutdown_signal().await?;
    tracing::info!("headless service stopping");
    state.server_stop().await?;
    Ok(())
}

#[cfg(unix)]
async fn shutdown_signal() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = terminate.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn shutdown_signal() -> Result<()> {
    tokio::signal::ctrl_c().await?;
    Ok(())
}

fn print_status() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build the status runtime")?;
    let report = runtime.block_on(status_report())?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_service_command(command: ServiceCommand) -> Result<()> {
    match command {
        ServiceCommand::Install { link } => {
            let operation = service::install(link.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
        }
        ServiceCommand::Uninstall => {
            let operation = service::uninstall()?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
        }
        ServiceCommand::Restart => {
            let operation = service::restart()?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
        }
        ServiceCommand::Status => print_status()?,
    }
    Ok(())
}

async fn status_report() -> Result<StatusReport> {
    let config = AppConfig::load();
    let service = service::status();
    let proxy = probe_proxy(&config).await;
    let ok = service.loaded && proxy.running && proxy.authenticated;

    Ok(StatusReport {
        version: env!("CARGO_PKG_VERSION"),
        ok,
        service,
        proxy,
        config: ConfigStatus::from(&config),
        codex: codex_status(&config),
    })
}

fn codex_status(config: &AppConfig) -> CodexStatus {
    let home = crate::codex::codex_home();
    let config_path = home.join("config.toml");
    let raw = std::fs::read_to_string(&config_path).unwrap_or_default();
    let config_exists = config_path.is_file();
    let config_parseable = toml::from_str::<toml::Value>(&raw).is_ok();
    let managed_block_present =
        raw.contains(crate::codex::BEGIN_MARK) && raw.contains(crate::codex::END_MARK);
    let managed_block_orphaned =
        raw.contains(crate::codex::BEGIN_MARK) && !raw.contains(crate::codex::END_MARK);
    let loom_dir = home.join("loom-router");
    let native_catalog = loom_dir.join("native-models.json");
    let merged_catalog = loom_dir.join("merged-models.json");
    let merged_model_count = std::fs::read_to_string(&merged_catalog)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("models")?.as_array().map(Vec::len))
        .unwrap_or(0);

    CodexStatus {
        home,
        config_path,
        config_exists,
        config_parseable,
        managed_block_present,
        managed_block_orphaned,
        integration_enabled: config.codex_integration && managed_block_present,
        native_catalog_present: native_catalog.is_file(),
        merged_catalog_present: merged_catalog.is_file(),
        merged_model_count,
    }
}

impl From<&AppConfig> for ConfigStatus {
    fn from(config: &AppConfig) -> Self {
        let enabled_providers: Vec<_> = config
            .providers
            .values()
            .filter(|provider| provider.enabled)
            .collect();
        let enabled_models: Vec<String> = enabled_providers
            .iter()
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(move |model| format!("{}/{}", provider.id, model.id))
            })
            .collect();

        Self {
            path: crate::config::config_path(),
            schema_version: config.schema_version,
            provider_proxies: config.provider_proxies.clone(),
            provider_count: config.providers.len(),
            enabled_provider_count: enabled_providers.len(),
            enabled_model_count: enabled_models.len(),
            enabled_models,
            codex_integration: config.codex_integration,
            native_slug_mode: config.native_slug_mode,
            active_model: config.active_model.clone(),
            side_call_fallback: config.side_call_fallback.clone(),
        }
    }
}

async fn probe_proxy(config: &AppConfig) -> ProxyStatus {
    let url = format!("http://127.0.0.1:{}/v1/models", config.port);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return ProxyStatus {
                running: false,
                authenticated: false,
                port: config.port,
                url,
                model_count: 0,
                models: Vec::new(),
                error: Some(error.to_string()),
            }
        }
    };
    match client
        .get(&url)
        .bearer_auth(crate::proxy::local_token())
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            if !status.is_success() {
                return ProxyStatus {
                    running: true,
                    authenticated: false,
                    port: config.port,
                    url,
                    model_count: 0,
                    models: Vec::new(),
                    error: Some(format!("HTTP {status}")),
                };
            }
            match response.json::<serde_json::Value>().await {
                Ok(payload) => {
                    let models: Vec<String> = payload
                        .get("data")
                        .and_then(serde_json::Value::as_array)
                        .map(|entries| {
                            entries
                                .iter()
                                .filter_map(|entry| {
                                    entry.get("id").and_then(serde_json::Value::as_str)
                                })
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    ProxyStatus {
                        running: true,
                        authenticated: true,
                        port: config.port,
                        url,
                        model_count: models.len(),
                        models,
                        error: None,
                    }
                }
                Err(error) => ProxyStatus {
                    running: true,
                    authenticated: true,
                    port: config.port,
                    url,
                    model_count: 0,
                    models: Vec::new(),
                    error: Some(format!("invalid JSON response: {error}")),
                },
            }
        }
        Err(error) => ProxyStatus {
            running: false,
            authenticated: false,
            port: config.port,
            url,
            model_count: 0,
            models: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_accepts_status_and_service_install() {
        let status = Cli::try_parse_from(["loom-router", "status"]).unwrap();
        assert!(matches!(status.command, Some(Command::Status)));

        let service = Cli::try_parse_from([
            "loom-router",
            "service",
            "install",
            "--link",
            "/opt/homebrew/bin/loom-router",
        ])
        .unwrap();
        assert!(matches!(
            service.command,
            Some(Command::Service {
                command: ServiceCommand::Install { link: Some(_) }
            })
        ));
    }

    #[test]
    fn config_status_omits_provider_credentials() {
        let mut config = AppConfig::default();
        config.providers.insert(
            "test".into(),
            crate::config::Provider {
                id: "test".into(),
                name: "Test".into(),
                protocol: crate::config::ProviderProtocol::OpenAI,
                base_url: "https://example.test/v1".into(),
                enabled: true,
                api_key: Some("super-secret".into()),
                keys: Vec::new(),
                rotation_enabled: false,
                has_key: false,
                context_window: None,
                user_agent: None,
                prompt_cache: None,
                models: Vec::new(),
            },
        );
        let json = serde_json::to_string(&ConfigStatus::from(&config)).unwrap();
        assert!(!json.contains("super-secret"));
        assert!(json.contains("\"provider_count\":1"));
    }
}
