// tmux control-mode client: one persistent pipe, commands in, framed
// responses out. Replaces one fork per tmux command with a write+read.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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

pub struct Tmux {
    child: Child,
    stdin: ChildStdin,
    rdr: BufReader<ChildStdout>,
}

impl Tmux {
    /// Attach a control-mode client. -f no-output: no %output notification
    /// per pane write — the key to staying idle between polls.
    pub fn connect() -> Result<Tmux, TmuxError> {
        let mut cmd = Command::new("tmux");
        // stay on the pane's server even on a non-default socket ($TMUX is
        // "socket_path,pid,session"); the var itself must go — a control
        // client is not a nested session
        if let Ok(tmux_env) = std::env::var("TMUX") {
            if let Some(sock) = tmux_env.split(',').next().filter(|s| !s.is_empty()) {
                cmd.arg("-S").arg(sock);
            }
        }
        let mut child = cmd
            .args(["-C", "attach-session", "-f", "no-output"])
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let rdr = BufReader::new(child.stdout.take().unwrap());
        let mut t = Tmux { child, stdin, rdr };
        // attach emits an unrequested greeting block — consume it so the
        // first run() doesn't pair with the wrong %begin
        t.read_block()?;
        Ok(t)
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

    /// Data already sitting in the BufReader — poll on fd() alone would miss it.
    pub fn buffered(&self) -> bool {
        !self.rdr.buffer().is_empty()
    }

    /// Consume queued notification lines without blocking; true when one of
    /// them signals a focus change (active pane/window/session moved).
    /// Stale %begin blocks (hook run-shell results) are consumed line by
    /// line here too — the next sync() barrier realigns the pipe anyway.
    pub fn drain_notifications(&mut self) -> Result<bool, TmuxError> {
        let mut focus = false;
        while self.buffered() || fd_readable(self.fd()) {
            let mut line = String::new();
            if self.read_line_retry(&mut line)? == 0 {
                return Err(TmuxError::Exited);
            }
            let l = line.trim_end_matches(['\n', '\r']);
            if l.starts_with("%exit") {
                return Err(TmuxError::Exited);
            }
            if l.starts_with("%window-pane-changed")
                || l.starts_with("%session-window-changed")
                || l.starts_with("%session-changed")
                || l.starts_with("%client-session-changed")
            {
                focus = true;
            }
        }
        Ok(focus)
    }

    /// read_line that survives EINTR: SIGWINCH lands mid-read and BufReader
    /// does not retry Interrupted — aborting here would desync the pipe
    /// (every later command pairs with the wrong response block).
    fn read_line_retry(&mut self, line: &mut String) -> std::io::Result<usize> {
        loop {
            match self.rdr.read_line(line) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                r => return r,
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
            // other notifications ignored in v1 (push upgrade hooks in here)
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
            body.push_str(l);
            body.push('\n');
        }
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

/// AGENTS_MON_DEBUG=<file>: free-form trace line (timings, counters).
pub fn debug_note(msg: &str) {
    let Ok(path) = std::env::var("AGENTS_MON_DEBUG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] # {msg}", std::process::id());
    }
}

/// AGENTS_MON_DEBUG=<file>: trace every command/response pair with timing.
fn debug_log(cmd: &str, r: &Result<String, TmuxError>, took: std::time::Duration) {
    let Ok(path) = std::env::var("AGENTS_MON_DEBUG") else {
        return;
    };
    let summary = match r {
        Ok(b) => format!("ok {}B {:?}", b.len(), b.chars().take(60).collect::<String>()),
        Err(e) => format!("ERR {e}"),
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
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
