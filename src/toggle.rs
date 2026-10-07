use crate::{panes, setup, tmux};
use std::collections::HashSet;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// stdout is a private one-byte startup acknowledgement, not a diagnostic log.
// Never relay child stderr to the user's terminal: it may contain configuration
// values or terminal controls. It goes to an owner-only file instead.
fn await_daemon(child: &mut Child) -> Result<(), &'static str> {
    let stdout = child
        .stdout
        .as_mut()
        .ok_or("daemon readiness channel unavailable")?;
    let fd = stdout.as_raw_fd();
    unsafe {
        if libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) < 0 {
            return Err("daemon readiness channel unavailable");
        }
    }
    // An active server with many panes can take longer than five seconds to scan.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child
            .try_wait()
            .map_err(|_| "daemon startup observation failed")?
            .is_some()
        {
            return Err("daemon exited during startup; check configuration and executable");
        }
        let mut byte = [0];
        match child.stdout.as_mut().unwrap().read(&mut byte) {
            Ok(1) if byte[0] == b'R' => return Ok(()),
            Ok(0) => return Err("daemon closed readiness channel before startup"),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            _ => return Err("invalid daemon readiness acknowledgement"),
        }
        if Instant::now() >= deadline {
            return Err("daemon readiness timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn run(plugin_dir: &Path, requested_mode: Option<&str>, requested_client: Option<&str>) -> i32 {
    // tmux ignores a TMUX value that names no socket and silently falls back to
    // the default server. A repro script with an empty socket variable must not
    // tear down and re-own the user's live sidebar.
    if let Ok(env) = std::env::var("TMUX") {
        if !env.is_empty() && !env.starts_with('/') {
            eprintln!(
                "agenmux: TMUX={env:?} names no socket; refusing to touch the default server"
            );
            return 1;
        }
    }
    // Closing an existing popup is a recovery path, including a broken file.
    if matches!(requested_mode, Some("popup") | None | Some("")) {
        let pin = tmux::runtime_dir().join("agenmux-pin");
        if pin.exists() {
            return if std::fs::remove_file(pin).is_ok() {
                0
            } else {
                1
            };
        }
    }
    // A fresh start may switch to a prepared release and re-enter it here.
    let gate = crate::autoupdate::gate(
        plugin_dir,
        crate::autoupdate::Entry::Toggle {
            mode: requested_mode.unwrap_or("").into(),
            client: requested_client.unwrap_or("").into(),
        },
    );
    let entry = crate::autoupdate::Entry::Toggle {
        mode: requested_mode.unwrap_or("").into(),
        client: requested_client.unwrap_or("").into(),
    };
    if let crate::autoupdate::Gate::Exit(code) = gate {
        return code;
    }
    let config = match crate::app_config::current(requested_mode) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            if let crate::autoupdate::Gate::Txn(txn) = gate {
                return txn.rollback();
            }
            return e.exit_code();
        }
    };
    // Held until this launch ends (a popup's whole lifetime); the daemon a
    // split launch starts inherits it.
    let (lease, txn) = match gate {
        crate::autoupdate::Gate::Lease(lease) => (Some(lease), None),
        crate::autoupdate::Gate::Txn(txn) => (None, Some(txn)),
        crate::autoupdate::Gate::Exit(code) => return code,
        crate::autoupdate::Gate::Open => match crate::autoupdate::lease(plugin_dir) {
            Ok(lease) => (lease, None),
            Err(crate::autoupdate::LeaseError::Updated) => {
                return crate::autoupdate::restart_installed(plugin_dir, entry);
            }
            Err(error) => {
                eprintln!("agenmux: {error}");
                return 1;
            }
        },
    };
    let client = requested_client
        .filter(|client| !client.is_empty())
        .map(str::to_string)
        .or_else(|| panes::newest_real_client("#{client_name}").ok().flatten());
    if config.mode == crate::app_config::DisplayMode::Popup {
        let (mut txn, mut held) = (txn, lease);
        let code = popup(plugin_dir, client, &config, &mut txn, &mut held);
        // The target's sidebar never drew a frame: keep the old release.
        match txn {
            Some(txn) => txn.rollback(),
            None => code,
        }
    } else {
        let held = txn
            .as_ref()
            .map(crate::autoupdate::Txn::lock)
            .or(lease.as_ref());
        let code = split(plugin_dir, client, &config, held);
        match txn {
            Some(txn) if code == 0 => drop(txn.commit()),
            Some(txn) => return txn.rollback(),
            None => {}
        }
        code
    }
}

fn option(name: &str) -> String {
    let canonical = tmux::command(&["show-option", "-gqv", name])
        .unwrap_or_default()
        .trim_end()
        .to_string();
    if !canonical.is_empty() {
        return canonical;
    }
    if !tmux::command(&["show-options", "-gq", name])
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return String::new();
    }
    let legacy = name
        .strip_prefix("@agenmux-")
        .map(|suffix| format!("@agents-mon-{suffix}"))
        .unwrap_or_else(|| name.to_string());
    tmux::command(&["show-option", "-gqv", &legacy])
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

/// Where the daemon's diagnostics go. A log that cannot be created falls back
/// to discarding them, as before; it never blocks the launch.
fn daemon_log() -> Stdio {
    crate::diag::create_daemon_log(&crate::diag::daemon_log_path())
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

/// `@agenmux-debug` reaches a daemon that tmux spawns, where a shell export
/// would not. An explicit environment variable still wins. Only an absolute
/// path is accepted: the daemon's working directory is nothing the user chose.
fn trace_file() -> Option<String> {
    if crate::compat_env("AGENMUX_DEBUG", "AGENTS_MON_DEBUG").is_some() {
        return None;
    }
    Some(option("@agenmux-debug")).filter(|path| path.starts_with('/'))
}

fn binary(plugin_dir: &Path) -> PathBuf {
    let configured = option("@agenmux-bin");
    if configured.is_empty() {
        plugin_dir.join("target/release/agenmux")
    } else {
        configured.into()
    }
}

fn control_alive() -> bool {
    let control = option("@agenmux-control-client");
    !control.is_empty()
        && tmux::lines(&["list-clients", "-F", "#{client_name}"])
            .is_ok_and(|clients| clients.iter().any(|client| client == &control))
        // control client up but FIFO gone = deaf daemon; treat as dead so the
        // caller tears down and starts fresh instead of re-selecting a zombie
        && tmux::runtime_dir().join("agenmux-keys").exists()
}
fn cli_mode(config: &crate::app_config::AppConfig) -> Option<&'static str> {
    (config.sources.get("display.mode").map(String::as_str) == Some("CLI")).then_some(match config
        .mode
    {
        crate::app_config::DisplayMode::Split => "split",
        crate::app_config::DisplayMode::Popup => "popup",
    })
}

fn split(
    plugin_dir: &Path,
    client: Option<String>,
    config: &crate::app_config::AppConfig,
    lease: Option<&crate::autoupdate::Lock>,
) -> i32 {
    let _lifecycle = match panes::lifecycle_lock() {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("agenmux: cannot acquire lifecycle lock: {error}");
            return 1;
        }
    };
    let window = client.as_deref().and_then(client_window);
    let mut reuse = option("@agenmux-on") == "1"
        && !option("@agenmux-generation").is_empty()
        && control_alive();
    if reuse {
        if panes::pane_add_config(window.as_deref(), config) != 0 {
            return 1;
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while window
            .as_deref()
            .is_some_and(|window| !window_sidebar_ready(window))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        reuse = option("@agenmux-on") == "1"
            && !option("@agenmux-generation").is_empty()
            && control_alive()
            && window.as_deref().is_none_or(window_sidebar_ready);
    }
    if !reuse {
        panes::stop_daemon();
        panes::teardown();
        let runtime = match tmux::prepare_runtime_dir() {
            Ok(dir) => dir,
            Err(error) => {
                eprintln!("agenmux: cannot prepare runtime directory: {error}");
                return 1;
            }
        };
        let generation = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        if tmux::command_status(&["set-option", "-g", "@agenmux-generation", &generation]).is_err()
            || tmux::command_status(&["set-option", "-g", "@agenmux-on", "1"]).is_err()
        {
            panes::clear_ownership(Some(&generation));
            return 1;
        }
        let bin = binary(plugin_dir);
        let mut child = None;
        let mut created = Vec::new();
        let new_files = ["agenmux-rows", "agenmux-scan-cache"]
            .map(|name| runtime.join(name))
            .into_iter()
            .filter(|path| !path.exists())
            .collect::<Vec<_>>();
        let result = (|| {
            let mut command = Command::new(&bin);
            command
                .arg("daemon")
                .env("AGENMUX_DIR", plugin_dir)
                .env("AGENMUX_STARTUP_ACK", "1")
                .env("AGENMUX_GENERATION", &generation)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(daemon_log());
            if let Some(trace) = trace_file() {
                command.env("AGENMUX_DEBUG", trace);
            }
            for (name, option) in [
                (
                    "AGENMUX_TMUX_ACTIVE_BORDER_STYLE",
                    "pane-active-border-style",
                ),
                ("AGENMUX_TMUX_WINDOW_STYLE", "window-style"),
            ] {
                if let Ok(style) = tmux::command(&["show-option", "-gv", option]) {
                    command.env(name, style.trim_end());
                }
            }
            if let Some(mode) = cli_mode(config) {
                command.env("AGENMUX_DISPLAY_OVERRIDE", mode);
            }
            if let Some(lease) = lease {
                lease.hand_down(&mut command, "AGENMUX_LEASE_FD");
            }
            child = Some(
                command
                    .spawn()
                    .map_err(|_| "cannot launch daemon; check executable")?,
            );
            // Startup is synchronous only for the requested window. Hooks add
            // sidebar readers lazily as real clients visit other windows.
            if panes::pane_add_record(window.as_deref(), config, &mut created) != 0 {
                return Err("cannot create startup pane");
            }
            await_daemon(child.as_mut().unwrap())?;
            if setup::run_config(plugin_dir, config) != 0 {
                return Err("daemon setup failed");
            }
            if child
                .as_mut()
                .unwrap()
                .try_wait()
                .map_err(|_| "cannot observe daemon")?
                .is_some()
            {
                return Err("daemon exited during setup");
            }
            Ok(())
        })();
        if let Err(error) = result {
            if let Some(child) = child.as_mut() {
                // Do not run the daemon's global teardown against newer resources.
                let _ = child.kill();
                let _ = child.wait();
            }
            let mut cleanup_failed = false;
            for (pane, window) in created {
                cleanup_failed |= tmux::command_status(&["kill-pane", "-t", &pane]).is_err();
                panes::restore_layout(&window);
                for prefix in ["@agenmux-layout-", "@agenmux-winsize-"] {
                    cleanup_failed |=
                        tmux::command_status(&["set-option", "-gu", &format!("{prefix}{window}")])
                            .is_err();
                }
            }
            if option("@agenmux-generation") == generation {
                cleanup_failed |= panes::clear_ownership(Some(&generation)) != 0;
            }
            if child.is_some() {
                for path in new_files.into_iter().chain([runtime.join("agenmux-keys")]) {
                    if let Err(e) = std::fs::remove_file(path) {
                        cleanup_failed |= e.kind() != std::io::ErrorKind::NotFound;
                    }
                }
                // A daemon killed before it said anything leaves an empty log;
                // one that reported why it died is the evidence worth keeping.
                let log = crate::diag::daemon_log_path();
                if std::fs::metadata(&log).is_ok_and(|meta| meta.len() == 0) {
                    let _ = std::fs::remove_file(log);
                }
            }
            eprintln!(
                "agenmux: {error}{}",
                if cleanup_failed {
                    "; startup cleanup incomplete"
                } else {
                    ""
                }
            );
            return 1;
        }
    }
    if option("@agenmux-nav-version") != setup::nav_version(config)
        && setup::run_config(plugin_dir, config) != 0
    {
        return 1;
    }
    select_sidebar(client.as_deref());
    0
}

fn window_sidebar_ready(window: &str) -> bool {
    tmux::lines(&[
        "list-panes",
        "-t",
        window,
        "-f",
        panes::IS_SIDEBAR,
        "-F",
        "#{pane_id}",
    ])
    .is_ok_and(|panes| {
        panes.into_iter().any(|pane| {
            tmux::command(&["capture-pane", "-p", "-t", &pane])
                .is_ok_and(|frame| !frame.trim().is_empty())
        })
    })
}

fn client_window(client: &str) -> Option<String> {
    tmux::command(&["display-message", "-p", "-c", client, "#{window_id}"])
        .ok()
        .map(|window| window.trim().to_string())
        .filter(|window| !window.is_empty())
}

fn select_sidebar(client: Option<&str>) {
    let Some(client) = client else { return };
    let Some(window) = client_window(client) else {
        return;
    };
    let pane = tmux::lines(&[
        "list-panes",
        "-t",
        &window,
        "-f",
        panes::IS_SIDEBAR,
        "-F",
        "#{pane_id}",
    ])
    .ok()
    .and_then(|panes| panes.into_iter().next());
    if let Some(pane) = pane.filter(|pane| !pane.is_empty()) {
        let _ = tmux::command_status(&["select-pane", "-t", &pane]);
    }
    let _ = tmux::command_status(&["switch-client", "-c", client, "-T", "agenmux"]);
}

/// `txn` is an activation to confirm once the first popup's sidebar has drawn
/// its first frame; confirming moves its lock into `held`, this launch's lease.
fn popup(
    plugin_dir: &Path,
    client: Option<String>,
    config: &crate::app_config::AppConfig,
    txn: &mut Option<crate::autoupdate::Txn>,
    held: &mut Option<crate::autoupdate::Lock>,
) -> i32 {
    let runtime = match tmux::prepare_runtime_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("agenmux: cannot prepare runtime directory: {error}");
            return 1;
        }
    };
    let pin = runtime.join("agenmux-pin");
    if pin.exists() {
        let _ = std::fs::remove_file(pin);
        return 0;
    }
    if std::fs::File::create(&pin).is_err() {
        return 1;
    }

    let dimension = |format| {
        let mut args = vec!["display-message", "-p"];
        if let Some(c) = client.as_deref() {
            args.extend(["-c", c]);
        }
        args.push(format);
        tmux::command(&args)
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok())
            .unwrap_or(10000)
            .max(1)
    };
    let mut settings = crate::app_config::LiveConfig::new(config.clone());
    let bin = binary(plugin_dir);
    // Multiple shell-command arguments use tmux's direct exec form. No shell
    // or format parser ever receives the executable path as command text.
    let jump = PathBuf::from(format!("{}.jump", pin.to_string_lossy()));

    while pin.exists() {
        // A popup jump reopens native geometry, not a render frame. Reuse this
        // process's file snapshot and keep last-valid live overrides on errors.
        settings.accept(crate::app_config::current(cli_mode(config)));
        let width = settings
            .settings
            .popup_width
            .min(dimension("#{client_width}"))
            .to_string();
        let height = match settings.settings.popup_height {
            crate::app_config::PopupHeight::Cells(n) => n as usize,
            _ => popup_height(&scan_cache(), client.as_deref()),
        }
        .min(dimension("#{client_height}") as usize)
        .to_string();
        let pin_env = format!("AGENMUX_PIN={}", pin.to_string_lossy());
        let mut args = vec![
            "display-popup".to_string(),
            "-E".to_string(),
            "-w".to_string(),
            width.clone(),
            "-h".to_string(),
            height.clone(),
            "-e".to_string(),
            pin_env,
            "-e".to_string(),
            format!("AGENMUX_DIR={}", plugin_dir.to_string_lossy()),
        ];
        if let Some(mode) = cli_mode(config) {
            args.extend(["-e".to_string(), format!("AGENMUX_DISPLAY_OVERRIDE={mode}")]);
        }
        if let Some(owner) = client.as_deref() {
            args.extend([
                "-c".to_string(),
                owner.to_string(),
                "-e".to_string(),
                format!("AGENMUX_POPUP_CLIENT={owner}"),
            ]);
        }
        if let Some(trace) = trace_file() {
            args.extend(["-e".to_string(), format!("AGENMUX_DEBUG={trace}")]);
        }
        let ready = runtime.join("agenmux-ready");
        if txn.is_some() {
            let _ = std::fs::remove_file(&ready);
            args.extend([
                "-e".to_string(),
                format!("AGENMUX_READY={}", ready.to_string_lossy()),
            ]);
        }
        args.push(bin.to_string_lossy().into_owned());
        args.push("sidebar".to_owned());
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let launched = match txn.take() {
            None => tmux::command_status(&refs).is_ok(),
            // display-popup blocks until the popup closes; confirm the
            // activation as soon as the sidebar inside reports its first frame.
            Some(pending) => match Command::new("tmux")
                .args(&refs)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Err(_) => {
                    *txn = Some(pending);
                    false
                }
                Ok(mut child) => {
                    let mut pending = Some(pending);
                    let status = loop {
                        if pending.is_some() && ready.exists() {
                            *held = pending.take().map(crate::autoupdate::Txn::commit);
                        }
                        match child.try_wait() {
                            Ok(Some(status)) => break status.success(),
                            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                            Err(_) => break false,
                        }
                    };
                    if pending.is_some() && ready.exists() {
                        *held = pending.take().map(crate::autoupdate::Txn::commit);
                    }
                    let _ = std::fs::remove_file(&ready);
                    *txn = pending;
                    status && txn.is_none()
                }
            },
        };
        if !launched {
            let _ = std::fs::remove_file(&jump);
            let _ = std::fs::remove_file(&pin);
            eprintln!("agenmux: popup launch failed; check executable and target client");
            return 1;
        }

        if jump.exists() {
            let target = std::fs::read_to_string(&jump).unwrap_or_default();
            let target = target.trim();
            let _ = std::fs::remove_file(&jump);
            if !target.is_empty() {
                let Some(owner) = client.as_deref() else {
                    let _ = std::fs::remove_file(&pin);
                    break;
                };
                if tmux::command_status(&["switch-client", "-c", owner, "-t", target]).is_err() {
                    let _ = std::fs::remove_file(&pin);
                    break;
                }
                let _ = tmux::command_status(&["select-window", "-t", target]);
                let _ = tmux::command_status(&["select-pane", "-t", target]);
            }
        } else {
            let _ = std::fs::remove_file(&pin);
            break;
        }
    }
    0
}

fn scan_cache() -> PathBuf {
    tmux::runtime_dir().join("agenmux-scan-cache")
}

fn popup_height(cache: &Path, client: Option<&str>) -> usize {
    let text = std::fs::read_to_string(cache).unwrap_or_default();
    let height = cache_height(&text).unwrap_or(15);
    let client_height = client
        .and_then(|client| {
            tmux::command(&["display-message", "-p", "-c", client, "#{client_height}"]).ok()
        })
        .or_else(|| tmux::command(&["display-message", "-p", "#{client_height}"]).ok())
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(height + 2);
    cap_popup_height(height, client_height)
}

fn cap_popup_height(height: usize, client_height: usize) -> usize {
    height.max(15).min(client_height.saturating_sub(2).max(1))
}

fn cache_height(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let mut sessions = HashSet::new();
    let mut rows = 0usize;
    let mut subjects = 0usize;
    for line in text.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        rows += 1;
        if fields.get(5).is_some_and(|subject| !subject.is_empty()) {
            subjects += 1;
        }
        if let Some(session) = fields
            .get(1)
            .and_then(|location| location.split(':').next())
        {
            sessions.insert(session);
        }
    }
    Some(rows + subjects + sessions.len() + 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_height_counts_rows_subjects_and_sessions() {
        let text = (0..10)
            .map(|i| format!("%{i}\ts{}:0.{i}\tcodex\tidle\t/tmp\tsubject", i % 2))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(cache_height(""), None);
        assert_eq!(cache_height(&text), Some(27));
    }

    #[test]
    fn popup_height_caps_to_client_and_keeps_help_floor() {
        assert_eq!(cap_popup_height(40, 22), 20);
        assert_eq!(cap_popup_height(10, 40), 15);
        assert_eq!(cap_popup_height(40, 10), 8);
    }
}
