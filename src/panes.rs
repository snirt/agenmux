use crate::tmux::{self, TmuxError};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const IS_SIDEBAR: &str = "#{||:#{==:#{pane_title},agenmux},#{==:#{@agenmux},1}}";

// tmux wait-for locks survive owner death and can hand off to cancelled
// waiters. The kernel instead releases this lock on close/SIGKILL. File uses
// CLOEXEC (including tmux children); do not unlink it, even after unlocking:
// a waiter may already have opened the inode.
pub(crate) struct ServerLock {
    _file: File,
}

impl ServerLock {
    fn acquire(server: &str, started: &str, scope: &str, wait: Duration) -> io::Result<Self> {
        let uid = unsafe { libc::geteuid() };
        // /tmp is system-owned, unlike caller-specific TMPDIR/HOME/runtime
        // options. All callers of one server must open the same inode.
        let directory = format!("/tmp/agenmux-pane-locks-{uid}");
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory)?;
        let metadata = dir.metadata()?;
        if metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            return Err(io::Error::other("unsafe pane lock directory"));
        }
        let name = std::ffi::CString::new(format!("{server}-{started}-{scope}.lock"))?;
        // Open relative to the validated directory descriptor, not its path.
        // NONBLOCK prevents a substituted FIFO from blocking before validation.
        // macOS returns a spurious ENOENT to the losers of a concurrent
        // O_CREAT race on the same name (roughly a third of eight racers);
        // the file exists by the next attempt.
        let mut fd = -1;
        for attempt in 0..20 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            fd = unsafe {
                libc::openat(
                    dir.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC
                        | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
                break;
            }
        }
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(io::Error::other("unsafe pane lock file"));
        }
        let deadline = Instant::now() + wait;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self { _file: file });
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "agenmux lock acquisition timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

pub(crate) fn lifecycle_lock() -> io::Result<ServerLock> {
    let identity = tmux::command(&["display-message", "-p", "#{pid}|#{start_time}"])
        .map_err(|error| io::Error::other(error.to_string()))?;
    let fields = identity.trim().split('|').collect::<Vec<_>>();
    let numeric = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    let [server, started] = fields.as_slice() else {
        return Err(io::Error::other("cannot resolve lifecycle lock identity"));
    };
    if !numeric(server) || !numeric(started) {
        return Err(io::Error::other("invalid lifecycle lock identity"));
    }
    ServerLock::acquire(server, started, "lifecycle", Duration::from_secs(30))
}

#[allow(dead_code)] // native toggle consumes this in the next migration task
pub fn newest_real_client(format: &str) -> Result<Option<String>, TmuxError> {
    let output_format = format!("#{{client_activity}}|{format}");
    let rows = tmux::lines(&[
        "list-clients",
        "-f",
        "#{?#{m:*control-mode*,#{client_flags}},0,1}",
        "-F",
        &output_format,
    ])?;
    Ok(newest_value(&rows))
}

fn newest_value(rows: &[String]) -> Option<String> {
    rows.iter()
        .filter_map(|row| {
            let (activity, value) = row.split_once('|')?;
            Some((activity.parse::<u64>().ok()?, value.to_string()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, value)| value)
}

// Restore tools respawn processless sidebar panes as idle shells; remove them before splitting.
fn kill_ghosts(win: &str, width: &str) {
    let default_shell =
        tmux::command(&["show-option", "-gqv", "default-shell"]).unwrap_or_default();
    let default_shell = default_shell.trim().rsplit('/').next().unwrap_or_default();
    let snapshot = crate::procs::Snapshot::take();
    let panes = tmux::lines(&[
        "list-panes",
        "-t",
        win,
        "-F",
        "#{pane_id}|#{pane_left}|#{pane_top}|#{pane_width}|#{pane_height}|#{window_height}|#{window_panes}|#{pane_pid}|#{pane_current_command}|#{pane_title}",
    ])
    .unwrap_or_default();
    for row in panes {
        let mut fields = row.split('|');
        let (
            Some(pane),
            Some("0"),
            Some("0"),
            Some(pane_width),
            Some(pane_height),
            Some(window_height),
            Some(window_panes),
            Some(pid),
            Some(command),
            Some(title),
        ) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        )
        else {
            continue;
        };
        let command = command
            .trim_start_matches('-')
            .rsplit('/')
            .next()
            .unwrap_or(command);
        let is_shell = matches!(
            command,
            "sh" | "bash" | "zsh" | "fish" | "nu" | "dash" | "ksh" | "tcsh" | "csh"
        ) || (!default_shell.is_empty() && command == default_shell);
        let (Ok(pid), Ok(window_panes)) = (pid.parse::<u32>(), window_panes.parse::<u32>()) else {
            continue;
        };
        if window_panes > 1
            && pane_width == width
            && pane_height == window_height
            && pid != 0
            && title != "agenmux"
            && is_shell
            && !snapshot.has_children(pid)
        {
            let _ = tmux::command_status(&["kill-pane", "-t", pane]);
        }
    }
}

pub fn pane_add(window: Option<&str>) -> i32 {
    match crate::app_config::current(None) {
        Ok(config) => pane_add_config(window, &config),
        Err(e) => {
            eprintln!("agenmux: {e}");
            e.exit_code()
        }
    }
}

pub fn pane_add_config(window: Option<&str>, config: &crate::app_config::AppConfig) -> i32 {
    pane_add_record(window, config, &mut Vec::new())
}

/// Record actual split-window results while holding the pane-add lock. Startup
/// rollback must not infer ownership from panes created by concurrent callers.
pub(crate) fn pane_add_record(
    window: Option<&str>,
    config: &crate::app_config::AppConfig,
    created: &mut Vec<(String, String)>,
) -> i32 {
    if !tmux::command(&["show-option", "-gqv", "@agenmux-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    // Resolve aliases before locking. Read identity from the contacted server,
    // not TMUX's possibly stale PID or a caller-selected temporary directory.
    let mut args = vec!["display-message", "-p"];
    if let Some(window) = window {
        args.extend(["-t", window]);
    }
    args.push("#{pid}|#{start_time}|#{window_id}");
    let identity = tmux::command(&args).unwrap_or_default();
    let fields = identity.trim().split('|').collect::<Vec<_>>();
    let numeric = |value: &str| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    let [server, started, win] = fields.as_slice() else {
        eprintln!("agenmux: cannot resolve pane lock identity");
        return 1;
    };
    if !numeric(server) || !numeric(started) || !win.strip_prefix('@').is_some_and(numeric) {
        eprintln!("agenmux: invalid pane lock identity");
        return 1;
    }
    let win = win.to_string();
    if tmux::command(&["display-message", "-p", "-t", &win, "#{session_name}"])
        .is_ok_and(|session| session.trim() == "pi")
    {
        return 0;
    }

    let _lock = match ServerLock::acquire(server, started, &win, Duration::from_secs(3)) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("agenmux: cannot acquire pane-add lock for {win}: {error}");
            return 1;
        }
    };
    if !tmux::command(&["show-option", "-gqv", "@agenmux-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    if tmux::lines(&[
        "list-panes",
        "-t",
        &win,
        "-f",
        IS_SIDEBAR,
        "-F",
        "#{pane_id}",
    ])
    .is_ok_and(|panes| !panes.is_empty())
    {
        return 0;
    }

    let width = bounded_width(config.sidebar_width, &win).to_string();
    kill_ghosts(&win, &width);
    let layout = match tmux::command(&["display-message", "-p", "-t", &win, "#{window_layout}"]) {
        Ok(layout) => layout.trim_end().to_string(),
        Err(_) => return 1,
    };
    let layout_option = format!("@agenmux-layout-{win}");
    if tmux::command_status(&["set-option", "-g", &layout_option, &layout]).is_err() {
        return 1;
    }

    let bin = match std::env::current_exe() {
        Ok(bin) => bin,
        Err(_) => return 1,
    };
    let runtime = format!("AGENMUX_RUNTIME_DIR={}", tmux::runtime_dir().display());
    let output = Command::new("tmux")
        .args([
            "split-window",
            "-hbf",
            "-d",
            "-l",
            &width,
            "-t",
            &win,
            "-P",
            "-F",
            "#{pane_id}",
            "-e",
            &runtime,
        ])
        .arg(bin)
        .arg("sidebar-pane")
        .stdin(Stdio::null())
        .output();
    let pane = match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            let _ = tmux::command_status(&["set-option", "-gu", &layout_option]);
            return 1;
        }
    };
    if pane.is_empty() {
        let _ = tmux::command_status(&["set-option", "-gu", &layout_option]);
        return 1;
    }
    created.push((pane.clone(), win));
    let _ = tmux::command_status(&["set-option", "-p", "-t", &pane, "allow-rename", "off"]);
    let _ = tmux::command_status(&["set-option", "-p", "-t", &pane, "@agenmux", "1"]);
    let _ = tmux::command_status(&["select-pane", "-t", &pane, "-T", "agenmux"]);
    0
}

pub fn pane_pin() -> i32 {
    let config = match crate::app_config::current(None) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let panes = tmux::lines(&["list-panes", "-a", "-f", IS_SIDEBAR, "-F", "#{pane_id}"])
        .unwrap_or_default();
    for pane in panes {
        let width = bounded_width(config.sidebar_width, &pane).to_string();
        let _ = tmux::command_status(&["resize-pane", "-t", &pane, "-x", &width]);
    }
    0
}

fn bounded_width(width: u16, target: &str) -> u16 {
    let available = tmux::command(&["display-message", "-p", "-t", target, "#{window_width}"])
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(width.saturating_add(2));
    width.min(available.saturating_sub(2).max(1))
}

pub fn pane_orphan() -> i32 {
    if !tmux::command(&["show-option", "-gqv", "@agenmux-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    let windows = tmux::lines(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}|#{window_panes}|#{session_id}",
    ])
    .unwrap_or_default();
    for row in windows {
        let mut fields = row.split('|');
        let (Some(win), Some("1"), Some(session)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !tmux::lines(&[
            "list-panes",
            "-t",
            win,
            "-f",
            IS_SIDEBAR,
            "-F",
            "#{pane_id}",
        ])
        .is_ok_and(|panes| !panes.is_empty())
        {
            continue;
        }

        let candidates = tmux::lines(&[
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_id}|#{window_last_flag}",
        ])
        .unwrap_or_default();
        let target = candidates
            .iter()
            .find_map(|row| {
                let (candidate, last) = row.split_once('|')?;
                (candidate != win && last == "1").then(|| candidate.to_string())
            })
            .or_else(|| {
                candidates.iter().find_map(|row| {
                    let candidate = row.split_once('|').map_or(row.as_str(), |(id, _)| id);
                    (candidate != win).then(|| candidate.to_string())
                })
            });
        let clients = tmux::lines(&[
            "list-clients",
            "-f",
            "#{?#{m:*control-mode*,#{client_flags}},0,1}",
            "-F",
            "#{client_name}",
        ])
        .unwrap_or_default();
        for client in clients.into_iter().filter(|client| !client.is_empty()) {
            let current = tmux::command(&["display-message", "-p", "-c", &client, "#{window_id}"])
                .unwrap_or_default();
            if current.trim() != win {
                continue;
            }
            if let Some(target) = &target {
                let _ = tmux::command_status(&["switch-client", "-c", &client, "-t", target]);
            } else if tmux::command_status(&["switch-client", "-c", &client, "-l"]).is_err() {
                let _ = tmux::command_status(&["switch-client", "-c", &client, "-p"]);
            }
        }
        let option = format!("@agenmux-layout-{win}");
        let _ = tmux::command_status(&["set-option", "-gu", &option]);
        let _ = tmux::command_status(&["kill-window", "-t", win]);
    }
    0
}

fn layout_size(layout: &str) -> Option<&str> {
    layout.split_once(',')?.1.split(',').next()
}

pub(crate) fn restore_layout(window: &str) {
    let option = format!("@agenmux-layout-{window}");
    let legacy_option = format!("@agents-mon-layout-{window}");
    let mut layout = tmux::command(&["show-option", "-gqv", &option]).unwrap_or_default();
    if layout.trim_end().is_empty() {
        layout = tmux::command(&["show-option", "-gqv", &legacy_option]).unwrap_or_default();
    }
    let layout = layout.trim_end();
    if layout.is_empty() {
        return;
    }
    let current = tmux::command(&[
        "display-message",
        "-p",
        "-t",
        window,
        "#{window_width}x#{window_height}",
    ])
    .unwrap_or_default();
    if layout_size(layout) == Some(current.trim()) {
        let _ = tmux::command_status(&["select-layout", "-t", window, layout]);
    }
}

/// Stop the running daemon and wait for its own cleanup to finish. Read the
/// ownership options first: `teardown` unsets them. A daemon left behind
/// dies on its next empty tick and runs the global `teardown` below, which
/// lands on whatever panes the next activation has created by then.
pub fn stop_daemon() {
    let option = |name: &str| {
        tmux::command(&["show-option", "-gqv", name])
            .unwrap_or_default()
            .trim_end()
            .to_string()
    };
    let client = option("@agenmux-control-client");
    let runtime = option("@agenmux-runtime-dir");
    if client.is_empty() {
        return;
    }
    // Losing its control pipe ends the daemon's loop; it removes the keys
    // FIFO as the last step of its teardown.
    let _ = tmux::command_status(&["detach-client", "-t", &client]);
    if runtime.is_empty() {
        return;
    }
    let keys = std::path::Path::new(&runtime).join("agenmux-keys");
    let deadline = Instant::now() + Duration::from_secs(3);
    while keys.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn teardown_panes() -> i32 {
    let sidebar_filter = ["#{||:", IS_SIDEBAR, ",#{==:#{pane_title},agents-mon}}"].concat();
    let panes = tmux::lines(&[
        "list-panes",
        "-a",
        "-f",
        &sidebar_filter,
        "-F",
        "#{pane_id}|#{window_id}",
    ])
    .unwrap_or_default();
    for row in panes {
        let Some((pane, window)) = row.split_once('|') else {
            continue;
        };
        let _ = tmux::command_status(&["kill-pane", "-t", pane]);
        restore_layout(window);
        let _ = std::fs::remove_file(crate::pane_writers::frame_path(&tmux::runtime_dir(), pane));
    }
    crate::setup::release_client_tables();

    let options = tmux::lines(&["show-options", "-g"]).unwrap_or_default();
    for row in options {
        let Some(option) = row.split_whitespace().next() else {
            continue;
        };
        if option.starts_with("@agenmux-layout-@")
            || option.starts_with("@agenmux-winsize-@")
            || option.starts_with("@agents-mon-layout-@")
            || option.starts_with("@agents-mon-winsize-@")
        {
            let _ = tmux::command_status(&["set-option", "-gu", option]);
        }
    }
    0
}

pub(crate) fn clear_ownership(generation: Option<&str>) -> i32 {
    if let Some(generation) = generation {
        let current =
            tmux::command(&["show-option", "-gqv", "@agenmux-generation"]).unwrap_or_default();
        if current.trim() != generation {
            return 0;
        }
    }
    for name in [
        "@agenmux-on",
        "@agenmux-control-client",
        "@agenmux-runtime-dir",
        "@agenmux-generation",
        "@agents-mon-on",
        "@agents-mon-control-client",
    ] {
        let _ = tmux::command_status(&["set-option", "-gu", name]);
    }
    0
}

pub fn teardown() -> i32 {
    teardown_panes();
    clear_ownership(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_client_ignores_invalid_rows_and_keeps_the_format_value() {
        let rows = vec![
            "10|first".to_string(),
            "bad|ignored".to_string(),
            "20|second value".to_string(),
        ];
        assert_eq!(newest_value(&rows).as_deref(), Some("second value"));
    }

    #[test]
    fn layout_size_is_the_absolute_window_size() {
        assert_eq!(layout_size("abcd,100x30,0,0[100x30,0,0,1]"), Some("100x30"));
        assert_eq!(layout_size("invalid"), None);
    }
}
