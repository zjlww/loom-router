use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

#[cfg(target_os = "linux")]
use std::process::Output;

#[cfg(any(target_os = "macos", test))]
const MACOS_SERVICE_LABEL: &str = "dev.loomrouter.agent";
#[cfg(target_os = "linux")]
const LINUX_SERVICE_UNIT: &str = "loom-router.service";

#[cfg(target_os = "macos")]
const SERVICE_REMOVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(target_os = "macos")]
const SERVICE_REMOVAL_POLL: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(target_os = "macos")]
const SERVICE_BOOTSTRAP_ATTEMPTS: usize = 10;
#[cfg(target_os = "macos")]
const SERVICE_BOOTSTRAP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, Serialize)]
pub(crate) struct ServiceStatus {
    pub(crate) platform: &'static str,
    pub(crate) manager: &'static str,
    pub(crate) label: &'static str,
    pub(crate) installed: bool,
    pub(crate) loaded: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) binary_path: PathBuf,
    pub(crate) definition_path: PathBuf,
    pub(crate) cli_path: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ServiceOperation {
    pub(crate) action: &'static str,
    pub(crate) platform: &'static str,
    pub(crate) manager: &'static str,
    pub(crate) label: &'static str,
    pub(crate) installed: bool,
    pub(crate) loaded: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) binary_path: PathBuf,
    pub(crate) definition_path: PathBuf,
    pub(crate) cli_path: Option<PathBuf>,
}

pub(crate) fn install(link: Option<&Path>) -> Result<ServiceOperation> {
    ensure_supported_platform()?;
    let source = std::env::current_exe().context("failed to locate the current executable")?;
    let binary = managed_binary_path();
    install_managed_binary(&source, &binary)?;
    platform_install(&binary)?;
    if let Some(link) = link {
        install_cli_link(link, &binary)?;
    }
    Ok(service_operation("install", link))
}

pub(crate) fn uninstall() -> Result<ServiceOperation> {
    platform_uninstall()?;
    Ok(service_operation("uninstall", None))
}

pub(crate) fn restart() -> Result<ServiceOperation> {
    platform_restart()?;
    std::thread::sleep(std::time::Duration::from_millis(500));
    Ok(service_operation("restart", None))
}

pub(crate) fn status() -> ServiceStatus {
    let binary_path = managed_binary_path();
    let definition_path = platform_definition_path();
    let installed = binary_path.is_file() && definition_path.is_file();
    let (loaded, pid) = platform_runtime_status().unwrap_or((false, None));
    ServiceStatus {
        platform: std::env::consts::OS,
        manager: platform_manager(),
        label: platform_label(),
        installed,
        loaded,
        pid,
        cli_path: find_cli_link(&binary_path),
        binary_path,
        definition_path,
    }
}

fn service_operation(action: &'static str, link: Option<&Path>) -> ServiceOperation {
    let service = status();
    ServiceOperation {
        action,
        platform: service.platform,
        manager: service.manager,
        label: service.label,
        installed: service.installed,
        loaded: service.loaded,
        pid: service.pid,
        binary_path: service.binary_path,
        definition_path: service.definition_path,
        cli_path: link
            .map(Path::to_path_buf)
            .or_else(|| find_cli_link(&managed_binary_path())),
    }
}

fn ensure_supported_platform() -> Result<()> {
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        Ok(())
    } else {
        bail!("service installation is supported on macOS and Linux only")
    }
}

// why: launchd owns service installation and process state on macOS. Keep the
// platform code in one branch so the shared CLI does not grow another layer of
// runtime conditionals.
#[cfg(target_os = "macos")]
fn platform_install(binary: &Path) -> Result<()> {
    install_launch_agent(binary)
}

#[cfg(target_os = "macos")]
fn platform_uninstall() -> Result<()> {
    uninstall_launch_agent()
}

#[cfg(target_os = "macos")]
fn platform_restart() -> Result<()> {
    restart_launch_agent()
}

#[cfg(target_os = "macos")]
fn platform_runtime_status() -> Result<(bool, Option<u32>)> {
    launchctl_status()
}

#[cfg(target_os = "macos")]
fn platform_definition_path() -> PathBuf {
    launch_agent_path()
}

#[cfg(target_os = "macos")]
fn platform_manager() -> &'static str {
    "launchd"
}

#[cfg(target_os = "macos")]
fn platform_label() -> &'static str {
    MACOS_SERVICE_LABEL
}

// why: Linux distributions use a user-scoped systemd manager for background
// services, which has no launchd equivalent. The unit remains per-user so
// credentials and Codex configuration stay in the operator's home.
#[cfg(target_os = "linux")]
fn platform_install(binary: &Path) -> Result<()> {
    install_systemd_unit(binary)
}

#[cfg(target_os = "linux")]
fn platform_uninstall() -> Result<()> {
    uninstall_systemd_unit()
}

#[cfg(target_os = "linux")]
fn platform_restart() -> Result<()> {
    run_systemctl(["restart", LINUX_SERVICE_UNIT]).context("systemctl restart failed")
}

#[cfg(target_os = "linux")]
fn platform_runtime_status() -> Result<(bool, Option<u32>)> {
    systemd_status()
}

#[cfg(target_os = "linux")]
fn platform_definition_path() -> PathBuf {
    systemd_unit_path()
}

#[cfg(target_os = "linux")]
fn platform_manager() -> &'static str {
    "systemd"
}

#[cfg(target_os = "linux")]
fn platform_label() -> &'static str {
    LINUX_SERVICE_UNIT
}

// why: service installation only has native backends for the two supported
// headless hosts. Windows can still run the foreground server, but it should
// fail explicitly instead of pretending a service was installed.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_install(_binary: &Path) -> Result<()> {
    bail!("service installation is supported on macOS and Linux only")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_uninstall() -> Result<()> {
    bail!("service management is supported on macOS and Linux only")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_restart() -> Result<()> {
    bail!("service management is supported on macOS and Linux only")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_runtime_status() -> Result<(bool, Option<u32>)> {
    Ok((false, None))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_definition_path() -> PathBuf {
    PathBuf::new()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_manager() -> &'static str {
    "unsupported"
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_label() -> &'static str {
    "loom-router"
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

fn managed_data_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    dirs::data_dir()
        .unwrap_or_else(|| fallback_data_dir(&home))
        .join("LoomRouter")
}

// why: dirs::data_dir has no platform-independent fallback. Keep the native
// convention for each supported UNIX so service install never writes under CWD.
fn fallback_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".local/share")
    }
}

fn managed_binary_path() -> PathBuf {
    managed_data_dir().join("bin/loom-router")
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/LaunchAgents")
        .join(format!("{MACOS_SERVICE_LABEL}.plist"))
}

#[cfg(target_os = "macos")]
fn install_launch_agent(binary: &Path) -> Result<()> {
    let plist = launch_agent_path();
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist, launch_agent_plist(binary)?)
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
    run_launchctl(["kickstart", "-k", service.as_str()]).context("launchctl kickstart failed")
}

#[cfg(target_os = "macos")]
fn uninstall_launch_agent() -> Result<()> {
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
    Ok(())
}

#[cfg(target_os = "macos")]
fn restart_launch_agent() -> Result<()> {
    let domain = launchctl_domain()?;
    let service = service_target(&domain);
    run_launchctl(["kickstart", "-k", service.as_str()]).context("launchctl kickstart failed")
}

#[cfg(target_os = "macos")]
fn launchctl_status() -> Result<(bool, Option<u32>)> {
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

#[cfg(target_os = "macos")]
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

#[cfg(any(target_os = "macos", test))]
fn service_target(domain: &str) -> String {
    format!("{domain}/{MACOS_SERVICE_LABEL}")
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn service_is_loaded(service: &str) -> bool {
    ProcessCommand::new("launchctl")
        .args(["print", service])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
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

#[cfg(any(target_os = "macos", test))]
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

#[cfg(target_os = "macos")]
fn launch_agent_plist(binary: &Path) -> Result<String> {
    let home = dirs::home_dir().context("failed to locate the home directory")?;
    let log_dir = home.join("Library/Logs/LoomRouter");
    std::fs::create_dir_all(&log_dir)?;
    let stdout = log_dir.join("loom-router.log");
    let stderr = log_dir.join("loom-router.err.log");
    Ok(render_launch_agent_plist(binary, &home, &stdout, &stderr))
}

#[cfg(any(target_os = "macos", test))]
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
        label = MACOS_SERVICE_LABEL,
        binary = xml_escape(&binary.display().to_string()),
        working_directory = xml_escape(&working_directory.display().to_string()),
        stdout = xml_escape(&stdout.display().to_string()),
        stderr = xml_escape(&stderr.display().to_string()),
    )
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("systemd/user")
        .join(LINUX_SERVICE_UNIT)
}

#[cfg(target_os = "linux")]
fn install_systemd_unit(binary: &Path) -> Result<()> {
    let unit_path = systemd_unit_path();
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&unit_path, render_systemd_unit(binary))
        .with_context(|| format!("failed to write {}", unit_path.display()))?;

    run_systemctl(["daemon-reload"]).context("systemctl daemon-reload failed")?;
    run_systemctl(["enable", LINUX_SERVICE_UNIT]).context("systemctl enable failed")?;
    run_systemctl(["restart", LINUX_SERVICE_UNIT]).context("systemctl restart failed")
}

#[cfg(target_os = "linux")]
fn uninstall_systemd_unit() -> Result<()> {
    let unit_path = systemd_unit_path();
    if unit_path.is_file() {
        let _ = run_systemctl(["disable", "--now", LINUX_SERVICE_UNIT]);
    }
    match std::fs::remove_file(&unit_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("failed to remove {}", unit_path.display()))
        }
    }
    let _ = run_systemctl(["daemon-reload"]);
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_status() -> Result<(bool, Option<u32>)> {
    let active = systemctl_output(["is-active", LINUX_SERVICE_UNIT])?;
    let loaded =
        active.status.success() && String::from_utf8_lossy(&active.stdout).trim() == "active";
    if !loaded {
        return Ok((false, None));
    }

    let output = systemctl_output(["show", "--property=MainPID", "--value", LINUX_SERVICE_UNIT])?;
    let pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid != 0);
    Ok((true, pid))
}

#[cfg(target_os = "linux")]
fn run_systemctl<const N: usize>(args: [&str; N]) -> Result<()> {
    let output = systemctl_output(args)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    bail!(
        "systemctl exited with {}: {} {}",
        output.status,
        stdout.trim(),
        stderr.trim()
    )
}

#[cfg(target_os = "linux")]
fn systemctl_output<const N: usize>(args: [&str; N]) -> Result<Output> {
    let mut command = ProcessCommand::new("systemctl");
    command.args(["--user", "--no-pager"]);
    command.args(args);
    command.output().context("failed to run systemctl")
}

#[cfg(any(target_os = "linux", test))]
fn render_systemd_unit(binary: &Path) -> String {
    format!(
        r#"[Unit]
Description=LoomRouter local model gateway
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
ExecStart={binary} serve
WorkingDirectory=%h
Environment="RUST_LOG=loom_router=info"
Restart=on-failure
RestartSec=5
NoNewPrivileges=true

[Install]
WantedBy=default.target
"#,
        binary = systemd_exec_arg(&binary.display().to_string())
    )
}

#[cfg(any(target_os = "linux", test))]
fn systemd_exec_arg(value: &str) -> String {
    let mut escaped = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            _ => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(target_os = "macos")]
fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("path is not valid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn systemd_unit_runs_the_headless_serve_command() {
        let unit = render_systemd_unit(Path::new(
            "/home/test/.local/share/LoomRouter/bin/loom-router",
        ));
        assert!(
            unit.contains("ExecStart=\"/home/test/.local/share/LoomRouter/bin/loom-router\" serve")
        );
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
    }

    #[test]
    fn systemd_unit_quotes_paths_and_escapes_specifiers() {
        assert_eq!(
            systemd_exec_arg("/home/test/A B/%config/loom-router"),
            "\"/home/test/A B/%%config/loom-router\""
        );
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
}
