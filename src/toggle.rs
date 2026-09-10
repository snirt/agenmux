use crate::{panes, setup, tmux};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

// stdout is a private one-byte startup acknowledgement, not a diagnostic log.
// Never relay child stderr: it may contain configuration values or terminal controls.
fn await_daemon(child: &mut Child) -> Result<(), &'static str> {
    let stdout = child.stdout.as_mut().ok_or("daemon readiness channel unavailable")?;
    let fd = stdout.as_raw_fd();
    unsafe {
        if libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) < 0 {
            return Err("daemon readiness channel unavailable");
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().map_err(|_| "daemon startup observation failed")?.is_some() {
            return Err("daemon exited during startup; check configuration and executable");
        }
        let mut byte = [0];
        match child.stdout.as_mut().unwrap().read(&mut byte) {
            Ok(1) if byte[0] == b'R' => return Ok(()),
            Ok(0) => return Err("daemon closed readiness channel before startup"),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {},
            _ => return Err("invalid daemon readiness acknowledgement"),
        }
        if Instant::now() >= deadline { return Err("daemon readiness timed out"); }
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
        let pin = std::env::temp_dir().join("agenmux-pin");
        if pin.exists() {
            return if std::fs::remove_file(pin).is_ok() {
                0
            } else {
                1
            };
        }
    }
    let config = match crate::app_config::current(requested_mode) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let client = requested_client
        .filter(|client| !client.is_empty())
        .map(str::to_string)
        .or_else(|| panes::newest_real_client("#{client_name}").ok().flatten());
    if config.mode == crate::app_config::DisplayMode::Popup {
        popup(plugin_dir, client, &config)
    } else {
        split(plugin_dir, client, &config)
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

fn split(plugin_dir: &Path, client: Option<String>, config: &crate::app_config::AppConfig) -> i32 {
    let window = client.as_deref().and_then(client_window);
    let mut reuse = option("@agenmux-on") == "1" && control_alive();
    if reuse {
        if panes::pane_add_config(window.as_deref(), config) != 0 {
            return 1;
        }
        // A close can remove its panes just before publishing @agenmux-on=off.
        // Let that short teardown finish instead of attaching to its dying daemon.
        std::thread::sleep(std::time::Duration::from_millis(100));
        reuse = option("@agenmux-on") == "1"
            && control_alive()
            && window.as_deref().is_none_or(window_has_sidebar);
    }
    if !reuse {
        panes::teardown();
        if tmux::command_status(&["set-option", "-g", "@agenmux-on", "1"]).is_err() {
            return 1;
        }
        let bin = binary(plugin_dir);
        let mut child = None;
        let mut created = Vec::new();
        let runtime = std::env::temp_dir();
        let new_files = ["agenmux-rows", "agenmux-scan-cache"]
            .map(|name| runtime.join(name))
            .into_iter().filter(|path| !path.exists()).collect::<Vec<_>>();
        let result = (|| {
            let mut windows = tmux::lines(&["list-windows", "-a", "-F", "#{window_id}"])
                .map_err(|_| "cannot enumerate startup windows")?;
            if let Some(focused) = window.as_deref() {
                if let Some(index) = windows.iter().position(|candidate| candidate == focused) {
                    let focused = windows.remove(index);
                    windows.insert(0, focused);
                }
            }
            child = Some(Command::new(&bin)
                .arg("daemon")
                .env("AGENMUX_DIR", plugin_dir)
                .env("AGENMUX_STARTUP_ACK", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn().map_err(|_| "cannot launch daemon; check executable")?);
            // Record only panes made by this activation, not arbitrary panes
            // that appear while the daemon is starting. The focused window goes
            // first so its sidebar can render while the remaining panes fan out.
            for window in windows {
                if panes::pane_add_record(Some(&window), config, &mut created) != 0 {
                    return Err("cannot create startup pane");
                }
            }
            await_daemon(child.as_mut().unwrap())?;
            if setup::run_config(plugin_dir, config) != 0 { return Err("daemon setup failed"); }
            if child.as_mut().unwrap().try_wait().map_err(|_| "cannot observe daemon")?.is_some() {
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
                    cleanup_failed |= tmux::command_status(&["set-option", "-gu", &format!("{prefix}{window}")]).is_err();
                }
            }
            for name in ["@agenmux-on", "@agenmux-control-client", "@agenmux-runtime-dir"] {
                cleanup_failed |= tmux::command_status(&["set-option", "-gu", name]).is_err();
            }
            if child.is_some() {
                for path in new_files.into_iter().chain([runtime.join("agenmux-keys")]) {
                    if let Err(e) = std::fs::remove_file(path) {
                        cleanup_failed |= e.kind() != std::io::ErrorKind::NotFound;
                    }
                }
            }
            eprintln!("agenmux: {error}{}", if cleanup_failed { "; startup cleanup incomplete" } else { "" });
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

fn window_has_sidebar(window: &str) -> bool {
    tmux::lines(&[
        "list-panes",
        "-t",
        window,
        "-f",
        panes::IS_SIDEBAR,
        "-F",
        "#{pane_id}",
    ])
    .is_ok_and(|panes| !panes.is_empty())
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

fn popup(plugin_dir: &Path, client: Option<String>, config: &crate::app_config::AppConfig) -> i32 {
    let pin = std::env::temp_dir().join("agenmux-pin");
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
        settings.accept(crate::app_config::current(None));
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
        if let Some(owner) = client.as_deref() {
            args.extend([
                "-c".to_string(),
                owner.to_string(),
                "-e".to_string(),
                format!("AGENMUX_POPUP_CLIENT={owner}"),
            ]);
        }
        args.push(bin.to_string_lossy().into_owned());
        args.push("sidebar".to_owned());
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        if tmux::command_status(&refs).is_err() {
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
    std::env::temp_dir().join("agenmux-scan-cache")
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
