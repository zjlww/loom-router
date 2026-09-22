use crate::config::AppConfig;
use crate::state::AppState;
use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;

const SERVICE_LABEL: &str = "dev.loomrouter.agent";
const CATALOG_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);
// why: launchd unloads a booted-out job asynchronously. It schedules cleanup a
// few seconds later and rejects a bootstrap of the same label with
// "Operation already in progress" (EALREADY) until that cleanup finishes, so an
// install that boots out and immediately re-bootstraps the agent fails unless we
// wait for the label to disappear first.
const SERVICE_REMOVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const SERVICE_REMOVAL_POLL: std::time::Duration = std::time::Duration::from_millis(100);
// why: even after the label is gone launchd can still refuse the first
// bootstrap that races a teardown it has not finished bookkeeping, so retry a
// bounded number of times instead of failing the install (and leaving the
// service down) on a transient error.
const SERVICE_BOOTSTRAP_ATTEMPTS: usize = 10;
const SERVICE_BOOTSTRAP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

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
    /// Manage the per-user macOS LaunchAgent.
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
    /// Unload and remove the LaunchAgent. Configuration and credentials stay.
    Uninstall,
    /// Restart the LaunchAgent.
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
struct ServiceStatus {
    platform: &'static str,
    label: &'static str,
    installed: bool,
    loaded: bool,
    pid: Option<u32>,
    binary_path: PathBuf,
    plist_path: PathBuf,
    cli_path: Option<PathBuf>,
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

#[derive(Debug, Serialize)]
struct ServiceOperation {
    action: &'static str,
    label: &'static str,
    installed: bool,
    loaded: bool,
    pid: Option<u32>,
    binary_path: PathBuf,
    plist_path: PathBuf,
    cli_path: Option<PathBuf>,
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
            let operation = install_service(link.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
        }
        ServiceCommand::Uninstall => {
            let operation = uninstall_service()?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
        }
        ServiceCommand::Restart => {
            let operation = restart_service()?;
            println!("{}", serde_json::to_string_pretty(&operation)?);
        }
        ServiceCommand::Status => print_status()?,
    }
    Ok(())
}

async fn status_report() -> Result<StatusReport> {
    let config = AppConfig::load();
    let service = service_status();
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

fn install_service(link: Option<&Path>) -> Result<ServiceOperation> {
    ensure_macos()?;
    let source = std::env::current_exe().context("failed to locate the current executable")?;
    let binary = managed_binary_path();
    install_managed_binary(&source, &binary)?;

    let plist = launch_agent_path();
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist, launch_agent_plist(&binary)?)
        .with_context(|| format!("failed to write {}", plist.display()))?;

    let domain = launchctl_domain()?;
    let service = service_target(&domain);
    let _ = run_launchctl(["bootout", service.as_str()]);
    wait_for_service_removal(&service);
    retry_launchctl(
        SERVICE_BOOTSTRAP_ATTEMPTS,
        SERVICE_BOOTSTRAP_RETRY_DELAY,
        || run_launchctl(["bootstrap", domain.as_str(), path_str(&plist)?]),
    )
    .context("launchctl bootstrap failed")?;
    run_launchctl(["kickstart", "-k", service.as_str()]).context("launchctl kickstart failed")?;

    if let Some(link) = link {
        install_cli_link(link, &binary)?;
    }
    Ok(service_operation("install", link))
}

fn uninstall_service() -> Result<ServiceOperation> {
    ensure_macos()?;
    let domain = launchctl_domain()?;
    let service = service_target(&domain);
    let _ = run_launchctl(["bootout", service.as_str()]);
    wait_for_service_removal(&service);
    let plist = launch_agent_path();
    match std::fs::remove_file(&plist) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to remove {}", plist.display()))
        }
    }
    Ok(service_operation("uninstall", None))
}

fn restart_service() -> Result<ServiceOperation> {
    ensure_macos()?;
    let domain = launchctl_domain()?;
    let service = service_target(&domain);
    run_launchctl(["kickstart", "-k", service.as_str()]).context("launchctl kickstart failed")?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(service_operation("restart", None))
}

fn service_operation(action: &'static str, link: Option<&Path>) -> ServiceOperation {
    let service = service_status();
    ServiceOperation {
        action,
        label: SERVICE_LABEL,
        installed: service.installed,
        loaded: service.loaded,
        pid: service.pid,
        binary_path: service.binary_path,
        plist_path: service.plist_path,
        cli_path: link
            .map(Path::to_path_buf)
            .or_else(|| find_cli_link(&managed_binary_path())),
    }
}

fn service_status() -> ServiceStatus {
    let binary_path = managed_binary_path();
    let plist_path = launch_agent_path();
    let installed = binary_path.is_file() && plist_path.is_file();
    let (loaded, pid) = launchctl_status().unwrap_or((false, None));
    ServiceStatus {
        platform: std::env::consts::OS,
        label: SERVICE_LABEL,
        installed,
        loaded,
        pid,
        cli_path: find_cli_link(&binary_path),
        binary_path,
        plist_path,
    }
}

fn launchctl_status() -> Result<(bool, Option<u32>)> {
    ensure_macos()?;
    let domain = launchctl_domain()?;
    let service = service_target(&domain);
    let output = ProcessCommand::new("launchctl")
        .args(["print", service.as_str()])
        .output()
        .context("failed to run launchctl print")?;
    if !output.status.success() {
        return Ok((false, None));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let pid = text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.trim().parse::<u32>().ok())
    });
    Ok((true, pid))
}

fn ensure_macos() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        bail!("LaunchAgent management is only supported on macOS")
    }
}

fn launchctl_domain() -> Result<String> {
    let output = ProcessCommand::new("id")
        .arg("-u")
        .output()
        .context("failed to determine the current uid")?;
    if !output.status.success() {
        bail!("id -u failed");
    }
    let uid = String::from_utf8(output.stdout)?.trim().to_string();
    if uid.is_empty() {
        bail!("id -u returned an empty uid");
    }
    Ok(format!("gui/{uid}"))
}

fn service_target(domain: &str) -> String {
    format!("{domain}/{SERVICE_LABEL}")
}

fn run_launchctl<const N: usize>(args: [&str; N]) -> Result<()> {
    let output = ProcessCommand::new("launchctl")
        .args(args)
        .output()
        .context("failed to run launchctl")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "launchctl exited with {}: {}{}",
        output.status,
        stdout.trim(),
        stderr.trim()
    )
}

fn service_is_loaded(service: &str) -> bool {
    ProcessCommand::new("launchctl")
        .args(["print", service])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn wait_for_service_removal(service: &str) -> bool {
    let deadline = std::time::Instant::now() + SERVICE_REMOVAL_TIMEOUT;
    while service_is_loaded(service) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SERVICE_REMOVAL_POLL);
    }
    true
}

fn retry_launchctl<F>(attempts: usize, delay: std::time::Duration, mut operation: F) -> Result<()>
where
    F: FnMut() -> Result<()>,
{
    let mut last_error = None;
    for _ in 0..attempts.max(1) {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                std::thread::sleep(delay);
            }
        }
    }
    Err(last_error.expect("attempts is at least one"))
}

fn install_managed_binary(source: &Path, destination: &Path) -> Result<()> {
    if source == destination {
        return Ok(());
    }
    let parent = destination
        .parent()
        .context("managed binary path has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.new",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("loom-router")
    ));
    std::fs::copy(source, &temporary).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            temporary.display()
        )
    })?;
    make_executable(&temporary)?;
    std::fs::rename(&temporary, destination)
        .with_context(|| format!("failed to install {}", destination.display()))?;
    Ok(())
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn install_cli_link(link: &Path, binary: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::symlink_metadata(link) {
            Ok(metadata) if metadata.file_type().is_symlink() => std::fs::remove_file(link)?,
            Ok(_) => bail!("refusing to replace non-symlink {}", link.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        symlink(binary, link)
            .with_context(|| format!("failed to create symlink {}", link.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (link, binary);
        bail!("CLI links are not supported on this platform")
    }
}

fn find_cli_link(binary: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    [
        PathBuf::from("/opt/homebrew/bin/loom-router"),
        home.join(".local/bin/loom-router"),
        home.join("bin/loom-router"),
    ]
    .into_iter()
    .find(|candidate| {
        std::fs::read_link(candidate)
            .map(|target| {
                let absolute = if target.is_absolute() {
                    target
                } else {
                    candidate
                        .parent()
                        .map(|parent| parent.join(&target))
                        .unwrap_or(target)
                };
                same_path(&absolute, binary)
            })
            .unwrap_or(false)
    })
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn launch_agent_plist(binary: &Path) -> Result<String> {
    let home = dirs::home_dir().context("failed to locate the home directory")?;
    let log_dir = home.join("Library/Logs/LoomRouter");
    std::fs::create_dir_all(&log_dir)?;
    let stdout = log_dir.join("loom-router.log");
    let stderr = log_dir.join("loom-router.err.log");
    Ok(render_launch_agent_plist(binary, &home, &stdout, &stderr))
}

fn render_launch_agent_plist(
    binary: &Path,
    working_directory: &Path,
    stdout: &Path,
    stderr: &Path,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>serve</string>
  </array>
  <key>WorkingDirectory</key>
  <string>{working_directory}</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>RUST_LOG</key>
    <string>loom_router=info</string>
  </dict>
</dict>
</plist>
"#,
        label = SERVICE_LABEL,
        binary = xml_escape(&binary.display().to_string()),
        working_directory = xml_escape(&working_directory.display().to_string()),
        stdout = xml_escape(&stdout.display().to_string()),
        stderr = xml_escape(&stderr.display().to_string()),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn managed_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Library/Application Support")
        })
        .join("LoomRouter")
}

fn managed_binary_path() -> PathBuf {
    managed_data_dir().join("bin/loom-router")
}

fn launch_agent_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist"))
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("path is not valid UTF-8")
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
    fn launch_agent_runs_the_headless_serve_command() {
        let plist = render_launch_agent_plist(
            Path::new("/Users/test/Library/Application Support/LoomRouter/bin/loom-router"),
            Path::new("/Users/test"),
            Path::new("/Users/test/Library/Logs/LoomRouter/loom-router.log"),
            Path::new("/Users/test/Library/Logs/LoomRouter/loom-router.err.log"),
        );
        assert!(plist.contains("<string>serve</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(!plist.contains("--window"));
    }

    #[test]
    fn service_target_is_scoped_to_the_launch_agent() {
        assert_eq!(service_target("gui/501"), "gui/501/dev.loomrouter.agent");
        assert_ne!(service_target("gui/501"), "gui/501");
    }

    #[test]
    fn retry_launchctl_recovers_from_transient_failures() {
        let mut calls = 0;
        let result = retry_launchctl(3, std::time::Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                bail!("transient launchctl failure");
            }
            Ok(())
        });

        assert!(result.is_ok());
        assert_eq!(calls, 3);
    }

    #[test]
    fn retry_launchctl_reports_the_last_error_once_the_budget_is_spent() {
        let mut calls = 0;
        let result = retry_launchctl(2, std::time::Duration::ZERO, || {
            calls += 1;
            bail!("Bootstrap failed: 5: Input/output error")
        });

        let error = result.expect_err("retry budget is exhausted");
        assert_eq!(calls, 2);
        assert!(error.to_string().contains("Bootstrap failed"));
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
