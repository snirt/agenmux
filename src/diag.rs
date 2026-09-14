// Diagnostics: an opt-in trace file for debugging and benchmarking, and the
// daemon's private error log. No crate dependencies: the release profile is
// size-tuned and two levels are all the runtime needs.
//
// Tiers, cheapest first:
// - the daemon's stderr lands in a capped file instead of /dev/null, so the
//   existing `eprintln!` diagnostics survive without emitting anything new;
// - `AGENMUX_DEBUG=<file>` (or the `@agenmux-debug` tmux option, relayed by
//   the launcher) appends a timestamped, scan-numbered trace;
// - lines that could carry pane content exist only in debug builds.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Daemon stderr destination inside the state directory. The previous
/// generation survives as `<name>.1`; nothing older is kept.
pub const DAEMON_LOG: &str = "daemon.log";
/// Above this the daemon log rotates: it only ever holds diagnostics, so a
/// reload that keeps failing must not fill the disk over a long session.
pub const DAEMON_LOG_LIMIT: u64 = 256 * 1024;

/// Formats every trace line when `crate::diag::enabled()`; otherwise costs one
/// atomic-free branch and no allocation.
macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::diag::enabled() {
            $crate::diag::trace(&format!($($arg)*));
        }
    };
}

/// Trace destination, resolved once per process. The daemon inherits it from
/// the launcher's environment, so a tmux option set after startup needs a
/// sidebar restart to take effect.
fn trace_path() -> Option<&'static Path> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = crate::compat_env("AGENMUX_DEBUG", "AGENTS_MON_DEBUG")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)?;
        append(
            &path,
            &line(
                0,
                &format!("# agenmux {} trace start", env!("CARGO_PKG_VERSION")),
            ),
        );
        Some(path)
    })
    .as_deref()
}

pub fn enabled() -> bool {
    trace_path().is_some()
}

static SCAN: AtomicU64 = AtomicU64::new(0);

/// Numbers the scan that starts now; every later trace line carries it, so
/// the tmux commands a scan issued can be attributed to it.
pub fn begin_scan() -> u64 {
    SCAN.fetch_add(1, Ordering::Relaxed) + 1
}

/// Free-form note (timings, counters, state changes), marked `# ` so a
/// reader can tell it from the tmux command lines around it.
pub fn trace(msg: &str) {
    trace_command(&format!("# {msg}"));
}

/// One tmux command/response line, written as given.
pub fn trace_command(msg: &str) {
    if let Some(path) = trace_path() {
        append(path, &line(SCAN.load(Ordering::Relaxed), msg));
    }
}

fn append(path: &Path, line: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// `[pid] HH:MM:SS.mmm s<scan> <msg>`: wall clock (UTC) so lines from the
/// daemon and one-shot commands writing the same file interleave in order.
fn line(scan: u64, msg: &str) -> String {
    format!("[{}] {} s{scan} {msg}", std::process::id(), clock())
}

fn clock() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_clock(now.as_secs(), now.subsec_millis())
}

fn format_clock(epoch_secs: u64, millis: u32) -> String {
    let day = epoch_secs % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{millis:03}",
        day / 3_600,
        day % 3_600 / 60,
        day % 60
    )
}

/// `$XDG_STATE_HOME/agenmux/daemon.log`, else `~/.local/state/agenmux/daemon.log`:
/// where the XDG spec puts logs, and where a reboot or a temp-dir sweep does
/// not erase the evidence of yesterday's failure. The runtime temp dir is the
/// fallback when neither root is usable.
pub fn daemon_log_path() -> PathBuf {
    let xdg = std::env::var_os("XDG_STATE_HOME");
    let home = std::env::var_os("HOME");
    state_dir(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
    .unwrap_or_else(std::env::temp_dir)
    .join(DAEMON_LOG)
}

/// Pure discovery, like the configuration file's: relative or empty roots
/// never make the daemon write next to whatever directory it happens to be in.
pub fn state_dir(xdg: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    xdg.filter(|p| p.is_absolute())
        .map(|p| p.join("agenmux"))
        .or_else(|| {
            home.filter(|p| p.is_absolute())
                .map(|p| p.join(".local/state/agenmux"))
        })
}

pub fn previous_daemon_log(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Exactly one prior generation survives, as `<log>.1`: the log a daemon
/// wrote replaces it, and a daemon that wrote nothing removes it. Anything
/// older is gone either way.
pub fn rotate_daemon_log(path: &Path) {
    let previous = previous_daemon_log(path);
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0) {
        let _ = std::fs::rename(path, previous);
    } else {
        let _ = std::fs::remove_file(previous);
    }
}

/// A fresh, owner-only log for a daemon about to start, after rotating the
/// previous one. Opened for append so every writer lands at the end even
/// after the file is replaced underneath it.
pub fn create_daemon_log(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    rotate_daemon_log(path);
    // std refuses append+truncate in one open; an empty leftover goes first
    let _ = std::fs::remove_file(path);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    // mode() only applies to a file created by this open
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Rotates the log once it outgrows `limit` and returns the fresh file the
/// daemon must adopt as stderr: its old descriptor still points at the
/// rotated inode. Called on the periodic tick: one metadata read every couple
/// of seconds.
pub fn cap_daemon_log(path: &Path, limit: u64) -> Option<std::fs::File> {
    if !std::fs::metadata(path).is_ok_and(|meta| meta.len() > limit) {
        return None;
    }
    let mut file = create_daemon_log(path).ok()?;
    let _ = writeln!(
        file,
        "agenmux: log restarted after exceeding {limit} bytes; previous generation kept as .1"
    );
    Some(file)
}

/// Points this process's stderr at `file`, so the diagnostics that follow a
/// rotation reach the new log instead of the rotated one.
pub fn adopt_stderr(file: &std::fs::File) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO);
    }
}

use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("agenmux-diag-{}-{name}", std::process::id()))
    }

    #[test]
    fn clock_formats_utc_time_of_day_with_milliseconds() {
        assert_eq!(format_clock(0, 0), "00:00:00.000");
        assert_eq!(format_clock(86_399, 999), "23:59:59.999");
        // 2026-09-14T12:34:56Z
        assert_eq!(format_clock(1_789_389_296, 7), "12:34:56.007");
    }

    #[test]
    fn trace_lines_carry_pid_clock_and_scan_number() {
        let line = line(42, "# scan 3ms");
        let mut fields = line.splitn(4, ' ');
        assert_eq!(fields.next().unwrap(), format!("[{}]", std::process::id()));
        let clock = fields.next().unwrap();
        assert_eq!(clock.len(), 12, "{clock}");
        assert_eq!(fields.next().unwrap(), "s42");
        assert_eq!(fields.next().unwrap(), "# scan 3ms");
    }

    fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(previous_daemon_log(path));
    }

    #[test]
    fn daemon_log_is_private_and_keeps_one_previous_generation() {
        let path = temp_path("daemon.log");
        let previous = previous_daemon_log(&path);
        std::fs::write(&previous, "two launches ago\n").unwrap();
        std::fs::write(&path, "previous daemon\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let mut file = create_daemon_log(&path).unwrap();
        writeln!(file, "agenmux: fresh").unwrap();
        drop(file);

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "agenmux: fresh\n");
        assert_eq!(
            std::fs::read_to_string(&previous).unwrap(),
            "previous daemon\n"
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        remove(&path);
    }

    #[test]
    fn silent_daemon_removes_the_older_generation() {
        let path = temp_path("silent.log");
        let previous = previous_daemon_log(&path);
        std::fs::write(&previous, "two launches ago\n").unwrap();
        std::fs::write(&path, "").unwrap();

        drop(create_daemon_log(&path).unwrap());

        assert!(!previous.exists());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        remove(&path);
    }

    #[test]
    fn appended_writes_land_at_the_end_after_truncation() {
        let path = temp_path("append.log");
        let mut file = create_daemon_log(&path).unwrap();
        writeln!(file, "first").unwrap();
        std::fs::File::create(&path).unwrap();
        writeln!(file, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        remove(&path);
    }

    #[test]
    fn daemon_log_rotates_only_past_its_limit() {
        let path = temp_path("cap.log");
        let previous = previous_daemon_log(&path);
        std::fs::write(&path, "x".repeat(10)).unwrap();
        assert!(cap_daemon_log(&path, 10).is_none());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 10);
        assert!(!previous.exists());

        std::fs::write(&path, "x".repeat(11)).unwrap();
        let fresh = cap_daemon_log(&path, 10).expect("rotated");
        drop(fresh);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("agenmux: log restarted"), "{content}");
        assert_eq!(std::fs::metadata(&previous).unwrap().len(), 11);
        remove(&path);
    }

    #[test]
    fn state_dir_prefers_absolute_xdg_root_then_home() {
        let xdg = Path::new("/xdg");
        let home = Path::new("/home/u");
        assert_eq!(
            state_dir(Some(xdg), Some(home)),
            Some(PathBuf::from("/xdg/agenmux"))
        );
        assert_eq!(
            state_dir(Some(Path::new("relative")), Some(home)),
            Some(PathBuf::from("/home/u/.local/state/agenmux"))
        );
        assert_eq!(
            state_dir(Some(Path::new("")), Some(home)),
            Some(PathBuf::from("/home/u/.local/state/agenmux"))
        );
        assert_eq!(state_dir(None, Some(Path::new("relative"))), None);
        assert_eq!(state_dir(None, None), None);
    }

    #[test]
    fn daemon_log_creates_its_private_directory() {
        let dir = temp_path("state-dir");
        let path = dir.join("nested").join("daemon.log");
        drop(create_daemon_log(&path).unwrap());
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_daemon_log_is_not_oversized() {
        assert!(cap_daemon_log(&temp_path("absent.log"), 0).is_none());
    }
}
