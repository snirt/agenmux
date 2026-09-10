// tmux control-mode client: one persistent pipe, commands in, framed
// responses out. Replaces one fork per tmux command with a write+read.
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const MAX_DRAIN_LINES: usize = 256;
const MAX_DRAIN_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct PendingChanges {
    pub panes: HashSet<String>,
    pub full: bool,
    pub focus: bool,
}

#[derive(Default)]
struct Notifications {
    pending: PendingChanges,
    attached_session: Option<String>,
}

impl Notifications {
    /// Returns true when `line` is an asynchronous control-mode notification.
    fn observe(&mut self, line: &str) -> bool {
        let line = line.trim_end_matches(['\n', '\r']);
        if let Some(pane) = line
            .strip_prefix("%output ")
            .or_else(|| line.strip_prefix("%extended-output "))
            .and_then(|rest| rest.split_whitespace().next())
        {
            self.pending.panes.insert(pane.to_string());
            return true;
        }
        if let Some(rest) = line.strip_prefix("%session-changed ") {
            self.attached_session = rest.split_whitespace().next().map(str::to_string);
            self.pending.full = true;
            self.pending.focus = true;
            return true;
        }
        if line.starts_with("%client-session-changed ") {
            self.pending.focus = true;
            return true;
        }
        if line.starts_with("%window-pane-changed")
            || line.starts_with("%session-window-changed")
            || line.starts_with("%layout-change")
        {
            self.pending.focus = true;
            return true;
        }
        if line.starts_with("%sessions-changed")
            || line.starts_with("%window-add")
            || line.starts_with("%window-close")
            || line.starts_with("%unlinked-window-add")
            || line.starts_with("%unlinked-window-close")
        {
            self.pending.full = true;
            return true;
        }
        line.starts_with("%client-detached ")
            || line.starts_with("%config-error ")
            || line.starts_with("%continue ")
            || line.starts_with("%message ")
            || line.starts_with("%pane-mode-changed ")
            || line.starts_with("%paste-buffer-changed ")
            || line.starts_with("%paste-buffer-deleted ")
            || line.starts_with("%pause ")
            || line.starts_with("%session-renamed ")
            || line.starts_with("%subscription-changed ")
            || line.starts_with("%unlinked-window-renamed ")
            || line.starts_with("%window-renamed ")
    }
}

pub enum TmuxError {
    /// Server gone or client detached — caller must clean up and exit 0
    /// (toggle.sh's popup loop relies on a clean sidebar exit).
    Exited,
    Error(String),
    Io(std::io::Error),
}

impl std::fmt::Display for TmuxError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TmuxError::Exited => write!(f, "tmux exited"),
            TmuxError::Error(e) => write!(f, "tmux: {e}"),
            TmuxError::Io(e) => write!(f, "tmux pipe: {e}"),
        }
    }
}

impl From<std::io::Error> for TmuxError {
    fn from(e: std::io::Error) -> Self {
        TmuxError::Io(e)
    }
}

pub fn command(args: &[&str]) -> Result<String, TmuxError> {
    let output = Command::new("tmux").args(args).output()?;
    if !output.status.success() {
        return Err(TmuxError::Error(
            String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string(),
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| TmuxError::Error(e.to_string()))
}

/// Where the daemon keeps its key FIFO and row map. The daemon publishes its
/// own temp dir as @agenmux-runtime-dir so key/click/wheel senders find it even
/// when tmux spawns them with a different TMPDIR than the daemon inherited.
pub fn runtime_dir() -> PathBuf {
    command(&["show-option", "-gqv", "@agenmux-runtime-dir"])
        .ok()
        .map(|dir| dir.trim_end().to_string())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn command_status(args: &[&str]) -> Result<(), TmuxError> {
    command(args).map(drop)
}

pub fn command_spawn(args: &[&str]) -> Result<(), TmuxError> {
    Command::new("tmux").args(args).spawn()?;
    Ok(())
}

#[allow(dead_code)] // setup/pane commands added in the next migration tasks consume this
pub fn lines(args: &[&str]) -> Result<Vec<String>, TmuxError> {
    command(args).map(|output| output.lines().map(str::to_string).collect())
}

#[allow(dead_code)] // setup commands added in the next migration tasks consume this
pub fn format_truth(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

#[allow(dead_code)] // setup commands added in the next migration tasks consume this
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub struct Tmux {
    child: Child,
    stdin: ChildStdin,
    rdr: BufReader<ChildStdout>,
    notifications: Notifications,
    partial_line: Vec<u8>,
}

impl Tmux {
    /// Attach a control-mode client. -f no-output: no %output notification
    /// per pane write — the key to staying idle between polls.
    pub fn connect() -> Result<Tmux, TmuxError> {
        Self::connect_with_output(false)
    }

    /// Attach the long-lived sidebar control client with pane output enabled.
    /// Output bytes remain in tmux; only pane IDs are retained as invalidations.
    pub fn connect_monitoring() -> Result<Tmux, TmuxError> {
        Self::connect_with_output(true)
    }

    fn connect_with_output(output: bool) -> Result<Tmux, TmuxError> {
        let mut cmd = Command::new("tmux");
        // stay on the pane's server even on a non-default socket ($TMUX is
        // "socket_path,pid,session"); the var itself must go — a control
        // client is not a nested session
        if let Ok(tmux_env) = std::env::var("TMUX") {
            if let Some(sock) = tmux_env.split(',').next().filter(|s| !s.is_empty()) {
                cmd.arg("-S").arg(sock);
            }
        }
        cmd.args(["-C", "attach-session"]);
        if !output {
            cmd.args(["-f", "no-output"]);
        }
        let mut child = cmd
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let rdr = BufReader::new(child.stdout.take().unwrap());
        let mut t = Tmux {
            child,
            stdin,
            rdr,
            notifications: Notifications::default(),
            partial_line: Vec::new(),
        };
        // attach emits an unrequested greeting block — consume it so the
        // first run() doesn't pair with the wrong %begin
        t.read_block()?;
        Ok(t)
    }

    pub fn attached_session(&self) -> Option<&str> {
        self.notifications.attached_session.as_deref()
    }

    pub fn pending_changes(&self) -> &PendingChanges {
        &self.notifications.pending
    }

    /// Take before scanning so notifications observed by `run` during the
    /// scan remain pending for the following pass.
    pub fn take_pending_changes(&mut self) -> PendingChanges {
        let mut pending = std::mem::take(&mut self.notifications.pending);
        if self.notifications.attached_session.is_none() {
            pending.full = true;
        }
        pending
    }

    /// Send one tmux command, return its output (without trailing newline
    /// handling — lines joined by \n).
    pub fn run(&mut self, cmd: &str) -> Result<String, TmuxError> {
        let t0 = std::time::Instant::now();
        self.stdin.write_all(cmd.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        let r = self.read_block();
        debug_log(cmd, &r, t0.elapsed());
        r
    }

    /// Resync barrier: discard any stale/unsolicited response blocks (hook
    /// run-shell results, etc.) until our marker echoes back. Makes the pipe
    /// self-healing — an off-by-one can survive at most one poll cycle.
    pub fn sync(&mut self) -> Result<(), TmuxError> {
        self.stdin.write_all(b"display-message -p am-sync\n")?;
        self.stdin.flush()?;
        loop {
            if self.read_block()? == "am-sync\n" {
                return Ok(());
            }
        }
    }

    /// Pipe fd for the caller's poll loop — readable means notifications
    /// (or a stale block) are queued.
    pub fn fd(&self) -> libc::c_int {
        self.rdr.get_ref().as_raw_fd()
    }

    /// PID tmux publishes as `#{client_pid}` for this control client.
    pub fn client_pid(&self) -> u32 {
        self.child.id()
    }

    /// Data already sitting in the BufReader — poll on fd() alone would miss it.
    pub fn buffered(&self) -> bool {
        self.rdr.buffer().contains(&b'\n')
    }

    /// Consume queued notification lines without blocking; true when one of
    /// them signals a focus change (active pane/window/session moved).
    /// Stale %begin blocks (hook run-shell results) are consumed line by
    /// line here too — the next sync() barrier realigns the pipe anyway.
    pub fn drain_notifications(&mut self) -> Result<bool, TmuxError> {
        let focus_before = self.notifications.pending.focus;
        let mut bytes_left = MAX_DRAIN_BYTES;
        for _ in 0..MAX_DRAIN_LINES {
            let Some(line) = self.try_read_line(&mut bytes_left)? else {
                break;
            };
            let l = line.trim_end_matches(['\n', '\r']);
            if l.starts_with("%exit") {
                return Err(TmuxError::Exited);
            }
            self.notifications.observe(l);
        }
        Ok(!focus_before && self.notifications.pending.focus)
    }

    /// Nonblocking line reader for event draining. Partial lines are retained
    /// without waiting for a newline; output payload storage is bounded.
    fn try_read_line(&mut self, bytes_left: &mut usize) -> Result<Option<String>, TmuxError> {
        loop {
            if *bytes_left == 0 {
                return Ok(None);
            }
            if let Some(end) = self.rdr.buffer().iter().position(|&b| b == b'\n') {
                let take = (end + 1).min(*bytes_left);
                if take <= end {
                    let prefix = self.rdr.buffer()[..take].to_vec();
                    append_line_prefix(&mut self.partial_line, &prefix);
                    self.rdr.consume(take);
                    *bytes_left -= take;
                    return Ok(None);
                }
                let mut tail = Vec::with_capacity(take);
                self.rdr.read_until(b'\n', &mut tail)?;
                *bytes_left -= tail.len();
                append_line_prefix(&mut self.partial_line, &tail);
                return Ok(Some(
                    String::from_utf8_lossy(&std::mem::take(&mut self.partial_line)).into_owned(),
                ));
            }
            if !self.rdr.buffer().is_empty() {
                let len = self.rdr.buffer().len().min(*bytes_left);
                let buffered = self.rdr.buffer()[..len].to_vec();
                append_line_prefix(&mut self.partial_line, &buffered);
                self.rdr.consume(len);
                *bytes_left -= len;
                continue;
            }
            if !fd_readable(self.fd()) {
                return Ok(None);
            }
            if self.rdr.fill_buf()?.is_empty() {
                return Err(TmuxError::Exited);
            }
        }
    }

    /// Read one protocol line as bytes. `%output` chunks may split a pane's
    /// multibyte UTF-8 sequence between lines, but only their ASCII prefix and
    /// pane ID are meaningful to us, so discard the payload before decoding.
    /// Keeping partial bytes undecoded also lets a bounded drain hand a
    /// mid-codepoint line back to the blocking response reader safely. Other
    /// lines remain strict UTF-8 so malformed command bodies still fail.
    fn read_line_retry(&mut self, line: &mut String) -> std::io::Result<usize> {
        let mut bytes = std::mem::take(&mut self.partial_line);
        loop {
            match self.rdr.read_until(b'\n', &mut bytes) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
                Ok(0) if bytes.is_empty() => return Ok(0),
                Ok(_) => {
                    let len = bytes.len();
                    line.push_str(&decode_protocol_line(bytes)?);
                    return Ok(len);
                }
            }
        }
    }

    /// Read until a complete %begin..%end/%error block; returns its body.
    /// Lines outside a block are notifications (%exit => Exited).
    fn read_block(&mut self) -> Result<String, TmuxError> {
        let mut line = String::new();
        // wait for %begin
        let tag = loop {
            line.clear();
            if self.read_line_retry(&mut line)? == 0 {
                return Err(TmuxError::Exited);
            }
            let l = line.trim_end_matches(['\n', '\r']);
            if let Some(rest) = l.strip_prefix("%begin ") {
                break block_tag(rest).to_string();
            }
            if l.starts_with("%exit") {
                return Err(TmuxError::Exited);
            }
            self.notifications.observe(l);
        };
        // collect body until the matching %end/%error (tag match guards
        // against pane content that happens to start with "%end")
        let mut body = String::new();
        loop {
            line.clear();
            if self.read_line_retry(&mut line)? == 0 {
                return Err(TmuxError::Exited);
            }
            let l = line.trim_end_matches(['\n', '\r']);
            if let Some(rest) = l.strip_prefix("%end ") {
                if block_tag(rest) == tag {
                    return Ok(body);
                }
            } else if let Some(rest) = l.strip_prefix("%error ") {
                if block_tag(rest) == tag {
                    return Err(TmuxError::Error(body.trim_end().to_string()));
                }
            }
            if !self.notifications.observe(l) {
                body.push_str(l);
                body.push('\n');
            }
        }
    }
}

/// Pane output is opaque transport data. Retain the notification header only;
/// every other control line is a command/metadata response and stays strict.
fn decode_protocol_line(mut bytes: Vec<u8>) -> std::io::Result<String> {
    for prefix in [b"%output ".as_slice(), b"%extended-output ".as_slice()] {
        if let Some(rest) = bytes.strip_prefix(prefix) {
            let pane_len = rest
                .iter()
                .position(|byte| byte.is_ascii_whitespace())
                .unwrap_or(rest.len());
            bytes.truncate(prefix.len() + pane_len);
            break;
        }
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Retain enough of a partial notification to identify it, but never retain
/// the potentially large escaped pane-output payload.
fn append_line_prefix(dst: &mut Vec<u8>, src: &[u8]) {
    const MAX_PREFIX: usize = 256;
    if dst.len() < MAX_PREFIX {
        dst.extend_from_slice(&src[..src.len().min(MAX_PREFIX - dst.len())]);
    }
}

/// fd readable right now (0ms poll)?
fn fd_readable(fd: libc::c_int) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut p, 1, 0) > 0 && p.revents & libc::POLLIN != 0 }
}

/// "%begin <time> <num> <flags>" -> num
fn block_tag(rest: &str) -> &str {
    rest.split_whitespace().nth(1).unwrap_or("")
}

/// AGENMUX_DEBUG=<file>: free-form trace line (timings, counters).
pub fn debug_note(msg: &str) {
    let Some(path) = crate::compat_env("AGENMUX_DEBUG", "AGENTS_MON_DEBUG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{}] # {msg}", std::process::id());
    }
}

/// AGENMUX_DEBUG=<file>: trace every command/response pair with timing.
fn debug_log(cmd: &str, r: &Result<String, TmuxError>, took: std::time::Duration) {
    let Some(path) = crate::compat_env("AGENMUX_DEBUG", "AGENTS_MON_DEBUG") else {
        return;
    };
    let summary = match r {
        Ok(b) => format!(
            "ok {}B {:?}",
            b.len(),
            b.chars().take(60).collect::<String>()
        ),
        Err(e) => format!("ERR {e}"),
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            f,
            "[{}] {}ms {:.60} -> {}",
            std::process::id(),
            took.as_millis(),
            cmd,
            summary
        );
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(b"detach-client\n");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scripted_tmux(script: &str) -> Tmux {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        Tmux {
            stdin: child.stdin.take().unwrap(),
            rdr: BufReader::new(child.stdout.take().unwrap()),
            child,
            notifications: Notifications::default(),
            partial_line: Vec::new(),
        }
    }

    #[test]
    fn quotes_shell_arguments_without_interpreting_tmux_metacharacters() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("can't"), "'can'\"'\"'t'");
        assert_eq!(quote("#,}"), "'#,}'");
        assert_eq!(quote("a\tb\nc"), "'a\tb\nc'");
    }

    #[test]
    fn tmux_truth_is_false_only_for_empty_and_zero() {
        assert!(!format_truth(""));
        assert!(!format_truth("0"));
        assert!(format_truth("1"));
        assert!(format_truth("off"));
        assert!(format_truth("00"));
        assert!(format_truth("\t\n"));
    }

    #[test]
    fn spawned_tmux_command_does_not_wait_for_completion() {
        let socket = format!("agenmux-spawn-test-{}", std::process::id());
        let status = Command::new("tmux")
            .args(["-L", &socket, "new-session", "-d"])
            .status()
            .unwrap_or_else(|error| panic!("could not start private tmux server: {error}"));
        assert!(status.success());

        let start = std::time::Instant::now();
        let result = command_spawn(&["-L", &socket, "run-shell", "sleep 3"]);
        let elapsed = start.elapsed();
        let _ = Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .status();

        assert!(result.is_ok());
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "{elapsed:?}"
        );
    }

    #[test]
    fn output_notifications_survive_before_and_during_a_command_response() {
        let mut tmux = scripted_tmux(
            "read _; printf '%s\\n' '%output %7 before' '%begin 1 2 0' \
             '%output %8 during' '%0\\trow' '%end 1 2 0'",
        );
        let body = match tmux.run("list-panes") {
            Ok(body) => body,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(body, "%0\\trow\n");
        assert_eq!(
            tmux.pending_changes().panes,
            HashSet::from(["%7".to_string(), "%8".to_string()])
        );
    }

    #[test]
    fn invalid_utf8_output_payloads_do_not_break_response_framing() {
        let mut tmux = scripted_tmux(
            "read _; printf '%%output %%7 \\377\\n%%begin 1 2 0\\n'; \
             printf '%%output %%8 \\376\\n%%0\\trow\\n%%end 1 2 0\\n'",
        );
        let body = match tmux.run("list-panes") {
            Ok(body) => body,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(body, "%0\trow\n");
        assert_eq!(
            tmux.pending_changes().panes,
            HashSet::from(["%7".to_string(), "%8".to_string()])
        );
    }

    #[test]
    fn invalid_utf8_command_body_remains_an_error() {
        let mut tmux =
            scripted_tmux("read _; printf '%%begin 1 2 0\\ninvalid \\377\\n%%end 1 2 0\\n'");
        assert!(matches!(tmux.run("synthetic"), Err(TmuxError::Io(error))
            if error.kind() == std::io::ErrorKind::InvalidData));
    }

    #[test]
    fn pane_rows_and_nonmatching_end_markers_remain_response_text() {
        let mut tmux = scripted_tmux(
            "read _; printf '%s\\n' '%begin 1 2 0' '%9\\tmetadata' \
             '%end 1 99' '%end 1 2 0'",
        );
        let body = match tmux.run("synthetic") {
            Ok(body) => body,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(body, "%9\\tmetadata\n%end 1 99\n");
    }

    #[test]
    fn session_notifications_invalidate_without_stealing_other_client_coverage() {
        let mut notifications = Notifications::default();
        assert!(notifications.observe("%session-changed $1 watched"));
        assert_eq!(notifications.attached_session.as_deref(), Some("$1"));
        notifications.pending = PendingChanges::default();

        assert!(notifications.observe("%client-session-changed /dev/pts/1 $2 other"));
        assert_eq!(notifications.attached_session.as_deref(), Some("$1"));
        assert!(notifications.pending.focus);
        assert!(!notifications.pending.full);
    }

    #[test]
    fn repeated_output_events_coalesce_by_pane() {
        let mut notifications = Notifications::default();
        notifications.observe("%output %3 first");
        notifications.observe("%output %3 second");
        notifications.observe("%extended-output %4 0 : third");
        assert_eq!(
            notifications.pending.panes,
            HashSet::from(["%3".to_string(), "%4".to_string()])
        );
    }

    #[test]
    fn draining_a_partial_notification_line_does_not_block() {
        let mut tmux = scripted_tmux("printf '%%output %%8 partial'; sleep 1");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !fd_readable(tmux.fd()) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let started = std::time::Instant::now();
        assert!(matches!(tmux.drain_notifications(), Ok(false)));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(tmux.pending_changes().panes.is_empty());
        let _ = tmux.child.kill();
    }

    #[test]
    fn response_read_joins_partial_notification_bytes_before_utf8_decoding() {
        let mut tmux = scripted_tmux(
            "printf '%%output %%8 \\342'; sleep .1; \
             read _; printf '\\202\\254\\n%%begin 1 2 0\\nok\\n%%end 1 2 0\\n'",
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !fd_readable(tmux.fd()) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(matches!(tmux.drain_notifications(), Ok(false)));

        let body = match tmux.run("synthetic") {
            Ok(body) => body,
            Err(error) => panic!("{error}"),
        };
        assert_eq!(body, "ok\n");
        assert_eq!(
            tmux.pending_changes().panes,
            HashSet::from(["%8".to_string()])
        );
    }

    #[test]
    fn draining_reports_eof_instead_of_spinning() {
        let mut tmux = scripted_tmux(":");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !fd_readable(tmux.fd()) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(matches!(tmux.drain_notifications(), Err(TmuxError::Exited)));
    }

    #[test]
    fn draining_a_newline_free_stream_has_a_byte_budget() {
        let mut tmux = scripted_tmux("printf '%070000d' 0; sleep 1");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !fd_readable(tmux.fd()) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let started = std::time::Instant::now();
        assert!(matches!(tmux.drain_notifications(), Ok(false)));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(tmux.partial_line.len() <= 256);
        let _ = tmux.child.kill();
    }
}
